//! Bounded OME-TIFF metadata: maps each OME `Image` series and its (C, Z, T)
//! planes to (member file, IFD) pairs. libvips keeps reading the pixels.
//!
//! - `detect`: OME-XML root in the first IFD's ImageDescription, or `*.companion.ome`.
//! - `open`: reads at most 32 MiB of OME-XML, preflights raw markup counts and
//!   reserves the parser's memory before parsing without DTDs. `BinaryOnly`
//!   follows its `MetadataFile`. Member files stay inside the opened file's
//!   canonical folder, and `TiffData` resolves per the OME-TIFF specification.
//!   The returned reservation covers the retained `Dataset`.
//! - `Series::plane`: the page of logical channel `c` (any RGB band) at `z`, `t`;
//!   `None` when the dataset does not store that plane.

use crate::channels::{domain, Channel, MAX_CHANNELS};
use crate::frames::{truncated_name, Calibration};
use crate::memory::{Budget, Reservation};
use crate::tiff_ifd::{self, number};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const MAX_XML_BYTES: u64 = 32 * 1024 * 1024;
/// Raw `<` and `=` counts size roxmltree's preallocation; also its node limit.
const MAX_MARKUP: usize = 1 << 21;
/// Raw `xmlns` substrings anywhere, bounding copied namespace scopes.
const MAX_XMLNS: usize = 64;
/// Conservative parser costs per node, attribute and namespace scopes.
const NODE_BYTES: u64 = 96;
const ATTRIBUTE_BYTES: u64 = 64;
const NAMESPACE_BYTES: u64 = 64 * 1024;
const MAX_IMAGES: usize = 4096;
/// Plane entries over all series and `TiffData`/`Plane` elements.
const MAX_PLANES: u64 = 1 << 20;
const MAX_MEMBERS: usize = 4096;
/// Main-chain IFDs walked over all members.
const MAX_IFDS: u64 = 1 << 21;
const MAX_IFD_ENTRIES: u64 = 1 << 16;
/// Bounded read of a member's ImageDescription for its root UUID.
const HEADER_BYTES: u64 = 64 * 1024;
/// OME UUIDs are `urn:uuid:` plus 36 characters; longer values are rejected.
const MAX_UUID_BYTES: usize = 256;
/// Canonical member paths longer than this are rejected.
const MAX_PATH_BYTES: usize = 4096;
/// Retained per member: summary, path, UUIDs and lookup entries.
const MEMBER_BYTES: u64 = 1024 + MAX_PATH_BYTES as u64 + 2 * MAX_UUID_BYTES as u64;

fn error(message: impl std::fmt::Display) -> String {
    format!("OME-TIFF: {message}")
}

#[derive(Clone, Debug)]
pub(crate) struct Dataset {
    /// Canonical member files inside the opened file's folder.
    pub members: Vec<PathBuf>,
    pub series: Vec<Series>,
    /// Series whose C/Z/T sizes, pixel type or samples differ from series 0.
    pub skipped: usize,
    /// Other files that defined the mapping: an opened companion or a
    /// `BinaryOnly` metadata file that is not itself a member.
    pub dependencies: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct Series {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub size_z: u32,
    pub size_t: u32,
    pub channels: Vec<Channel>,
    pub rgb: bool,
    pub sample_bits: u8,
    pub calibration: Calibration,
    /// Indexed `c + C·(z + Z·t)` over logical channels (one for RGB).
    pub planes: Vec<Option<PlaneRef>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlaneRef {
    pub member: u16,
    pub ifd: u32,
}

impl Series {
    fn logical_channels(&self) -> u32 {
        if self.rgb {
            1
        } else {
            self.channels.len() as u32
        }
    }

    pub fn plane(&self, c: u32, z: u32, t: u32) -> Option<PlaneRef> {
        if c as usize >= self.channels.len() || z >= self.size_z || t >= self.size_t {
            return None;
        }
        let (c, count) = if self.rgb {
            (0, 1)
        } else {
            (c, self.logical_channels())
        };
        let index =
            (c as usize) + count as usize * (z as usize + self.size_z as usize * t as usize);
        self.planes.get(index).copied().flatten()
    }
}

/// A companion, a TIFF whose first description has an OME root, or one whose
/// description mentions OME-XML that the root probe cannot place, so it gets
/// the OME path's explicit errors instead of the editable ordinary-TIFF path.
pub(crate) fn detect(path: &Path) -> Result<bool, String> {
    if is_companion(path) {
        return Ok(true);
    }
    let Some(tiff) = Tiff::open(path)? else {
        return Ok(false);
    };
    Ok(match tiff.root() {
        // An XML prolog the probe cannot finish is never treated as ordinary.
        Ok(Root::Ome(_) | Root::Unknown) => true,
        _ => tiff.mentions_ome()?,
    })
}

pub(crate) fn open(path: &Path, budget: &Arc<Budget>) -> Result<(Dataset, Reservation), String> {
    let path = std::fs::canonicalize(path)
        .map_err(|e| error(format!("cannot open {}: {e}", path.display())))?;
    let companion = is_companion(&path);
    let mut origin = Origin {
        directory: path
            .parent()
            .ok_or_else(|| error("the dataset has no folder."))?
            .to_path_buf(),
        opened: (!companion).then(|| path.clone()),
        opened_uuid: None,
        xml_in_opened: !companion,
        expected_uuid: None,
    };
    let (read, text) = if companion {
        read_file(&path, budget)?
    } else {
        let tiff = Tiff::open(&path)?.ok_or_else(|| error("the file is not a TIFF."))?;
        let (offset, len) = tiff
            .description()?
            .ok_or_else(|| error("the first TIFF page has no OME-XML description."))?;
        read_text(len, budget, |bytes| tiff.read(offset, bytes))?
    };
    let (name, uuid) = match from_xml(&text, budget, &mut origin)? {
        Metadata::Dataset(dataset, reservation) => {
            return Ok((with_dependencies(dataset, [path]), reservation))
        }
        Metadata::BinaryOnly(name, uuid) => (name, uuid),
    };
    drop(text);
    drop(read);
    origin.xml_in_opened = false;
    origin.expected_uuid = uuid;
    // The full metadata is a companion XML file or another OME-TIFF's description.
    let metadata = confined(&origin.directory, &name)?;
    let (_read, text) = match Tiff::open(&metadata)? {
        Some(tiff) => {
            let (offset, len) = tiff
                .description()?
                .ok_or_else(|| error(format!("{name} has no OME-XML description.")))?;
            read_text(len, budget, |bytes| tiff.read(offset, bytes))?
        }
        None => read_file(&metadata, budget)?,
    };
    match from_xml(&text, budget, &mut origin)? {
        Metadata::Dataset(dataset, reservation) => {
            Ok((with_dependencies(dataset, [path, metadata]), reservation))
        }
        Metadata::BinaryOnly(..) => unreachable!("BinaryOnly requires XML in the opened TIFF"),
    }
}

/// At most two paths, covered by the dataset's fixed reservation allowance.
fn with_dependencies<const N: usize>(mut dataset: Dataset, files: [PathBuf; N]) -> Dataset {
    for file in files {
        if !dataset.members.contains(&file) && !dataset.dependencies.contains(&file) {
            dataset.dependencies.push(file);
        }
    }
    dataset
}

pub(crate) fn is_companion(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".companion.ome"))
}

/// Where the XML came from and which file a reference without `FileName` means.
struct Origin {
    directory: PathBuf,
    /// The opened TIFF; `None` for a companion opened directly.
    opened: Option<PathBuf>,
    opened_uuid: Option<String>,
    xml_in_opened: bool,
    /// `BinaryOnly` UUID of the metadata file.
    expected_uuid: Option<String>,
}

enum Metadata {
    Dataset(Dataset, Reservation),
    /// `MetadataFile` and `UUID`.
    BinaryOnly(String, Option<String>),
}

fn read_text(
    len: u64,
    budget: &Arc<Budget>,
    read: impl FnOnce(&mut [u8]) -> Result<(), String>,
) -> Result<(Reservation, String), String> {
    if len > MAX_XML_BYTES {
        return Err(error("OME-XML exceeds the 32 MiB limit."));
    }
    let reservation = budget.require(len, &budget_error())?;
    let mut bytes = vec![0; len as usize];
    read(&mut bytes)?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    let text = String::from_utf8(bytes).map_err(|_| error("OME-XML is not valid UTF-8."))?;
    Ok((reservation, text))
}

fn read_file(path: &Path, budget: &Arc<Budget>) -> Result<(Reservation, String), String> {
    let mut file = File::open(path).map_err(error)?;
    let len = file.metadata().map_err(error)?.len();
    read_text(len, budget, |bytes| file.read_exact(bytes).map_err(error))
}

fn budget_error() -> String {
    error("metadata does not fit the application memory budget.")
}

/// Preflights raw markup, reserves the parser's peak and parses without DTDs.
fn parse<'a>(text: &'a str, budget: &Arc<Budget>) -> Result<(Reservation, Document<'a>), String> {
    let bytes = text.as_bytes();
    let count = |byte: u8| bytes.iter().filter(|b| **b == byte).count();
    let (nodes, attributes) = (count(b'<'), count(b'='));
    if nodes > MAX_MARKUP || attributes > MAX_MARKUP {
        return Err(error(format!(
            "OME-XML has more than {MAX_MARKUP} elements or attributes."
        )));
    }
    if bytes.windows(5).filter(|w| *w == b"xmlns").count() > MAX_XMLNS {
        return Err(error(format!(
            "OME-XML declares more than {MAX_XMLNS} namespaces."
        )));
    }
    let reservation = budget.require(
        nodes as u64 * 2 * NODE_BYTES
            + attributes as u64 * 2 * ATTRIBUTE_BYTES
            + 2 * bytes.len() as u64
            + NAMESPACE_BYTES,
        &budget_error(),
    )?;
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_MARKUP as u32,
        ..Default::default()
    };
    let document = Document::parse_with_options(text, options).map_err(|e| match e {
        roxmltree::Error::DtdDetected => error("OME-XML with a DTD is not supported."),
        roxmltree::Error::NodesLimitReached => {
            error(format!("OME-XML has more than {MAX_MARKUP} nodes."))
        }
        e => error(format!("invalid OME-XML: {e}")),
    })?;
    Ok((reservation, document))
}

fn from_xml(text: &str, budget: &Arc<Budget>, origin: &mut Origin) -> Result<Metadata, String> {
    let (_reservation, document) = parse(text, budget)?;
    let root = document.root_element();
    if root.tag_name().name() != "OME" {
        return Err(error("the XML root is not an OME element."));
    }
    let uuid = root.attribute("UUID").map(checked_uuid).transpose()?;
    if origin.xml_in_opened {
        origin.opened_uuid = uuid.map(str::to_owned);
    }
    if let (Some(expected), Some(uuid)) = (&origin.expected_uuid, uuid) {
        if expected != uuid {
            return Err(error(
                "the BinaryOnly metadata file UUID does not match its reference.",
            ));
        }
    }
    if let Some(binary) = children(root, "BinaryOnly").next() {
        if !origin.xml_in_opened {
            return Err(error(
                "BinaryOnly must be inside an OME-TIFF and point to the full metadata.",
            ));
        }
        let name = binary
            .attribute("MetadataFile")
            .ok_or_else(|| error("BinaryOnly has no MetadataFile."))?;
        return Ok(Metadata::BinaryOnly(
            name.into(),
            binary
                .attribute("UUID")
                .map(checked_uuid)
                .transpose()?
                .map(str::to_owned),
        ));
    }
    let (dataset, reservation) = build(root, origin, budget)?;
    Ok(Metadata::Dataset(dataset, reservation))
}

fn children<'a, 'd>(node: Node<'a, 'd>, name: &'static str) -> impl Iterator<Item = Node<'a, 'd>> {
    node.children()
        .filter(move |n| n.is_element() && n.tag_name().name() == name)
}

fn attribute<T: std::str::FromStr>(node: Node, key: &str) -> Result<Option<T>, String> {
    node.attribute(key)
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|_| error(format!("invalid {key} value {value:?}.")))
        })
        .transpose()
}

fn build(
    root: Node,
    origin: &Origin,
    budget: &Arc<Budget>,
) -> Result<(Dataset, Reservation), String> {
    let images = children(root, "Image").count();
    if images == 0 {
        return Err(error("the OME-XML contains no images."));
    }
    if images > MAX_IMAGES {
        return Err(error(format!("more than {MAX_IMAGES} images.")));
    }
    let elements = root
        .descendants()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "TiffData" | "Plane"))
        .count();
    if elements as u64 > MAX_PLANES {
        return Err(error(format!(
            "more than {MAX_PLANES} TiffData and Plane elements."
        )));
    }
    // Series 0 defines the browsing dimensions; XY sizes may differ.
    let mut kept: Vec<(Series, Node, [usize; 3])> = Vec::new();
    let mut skipped = 0;
    for (index, image) in children(root, "Image").enumerate() {
        match describe(index, image) {
            Ok(series)
                if kept
                    .first()
                    .is_none_or(|first| compatible(&first.0, &series.0)) =>
            {
                kept.push(series)
            }
            Err(message) if index == 0 => return Err(message),
            _ => skipped += 1,
        }
    }
    // Each series is already within MAX_PLANES; the sum cannot overflow.
    let total = kept
        .iter()
        .try_fold(0u64, |total, (series, ..)| {
            total
                .checked_add(plane_count(series)?)
                .filter(|total| *total <= MAX_PLANES)
        })
        .ok_or_else(|| error(format!("more than {MAX_PLANES} planes.")))?;
    // Member summaries are allocated while resolving references; reserve their
    // bounded size first. The retained reservation below replaces this one.
    let references = kept
        .iter()
        .map(|(_, pixels, _)| children(*pixels, "TiffData").count() as u64)
        .sum::<u64>()
        .clamp(1, MAX_MEMBERS as u64);
    let workspace = budget.require(references * MEMBER_BYTES, &budget_error())?;
    let mut members = Members {
        origin,
        files: Vec::new(),
        paths: HashMap::new(),
        uuids: HashMap::new(),
        references: HashMap::new(),
        walked: 0,
    };
    for (_, pixels, _) in &kept {
        for data in children(*pixels, "TiffData") {
            members.resolve(reference(data))?;
        }
    }
    let entry = std::mem::size_of::<Option<PlaneRef>>() as u64;
    // 2 × MAX_PATH_BYTES covers up to two dependency paths.
    let retained = 4096
        + 2 * MAX_PATH_BYTES as u64
        + total * entry
        + kept.iter().map(|(series, ..)| bytes(series)).sum::<u64>()
        + members
            .files
            .iter()
            .map(|file| (std::mem::size_of::<PathBuf>() + file.path.as_os_str().len()) as u64)
            .sum::<u64>();
    let reservation = budget.require(retained, &budget_error())?;
    drop(workspace);
    let mut assigned = 0;
    let series = kept
        .into_iter()
        .map(|(mut series, pixels, order)| {
            series.planes = planes(&series, pixels, order, &mut members, &mut assigned)?;
            Ok(series)
        })
        .collect::<Result<Vec<_>, String>>()?;
    if series
        .iter()
        .all(|series| series.planes.iter().all(Option::is_none))
    {
        return Err(error("no image planes are stored in TIFF pages."));
    }
    let members = members.files.into_iter().map(|file| file.path).collect();
    Ok((
        Dataset {
            members,
            series,
            skipped,
            dependencies: Vec::new(),
        },
        reservation,
    ))
}

/// Logical planes of a series; `describe` keeps this within `MAX_PLANES`.
fn plane_count(series: &Series) -> Option<u64> {
    u64::from(series.logical_channels())
        .checked_mul(u64::from(series.size_z))?
        .checked_mul(u64::from(series.size_t))
        .filter(|count| *count <= MAX_PLANES)
}

fn bytes(series: &Series) -> u64 {
    (std::mem::size_of::<Series>()
        + series.name.len()
        + series
            .channels
            .iter()
            .map(|c| std::mem::size_of::<Channel>() + c.name.len() + c.color.len())
            .sum::<usize>()) as u64
}

fn compatible(first: &Series, other: &Series) -> bool {
    (
        first.channels.len(),
        first.rgb,
        first.size_z,
        first.size_t,
        first.sample_bits,
    ) == (
        other.channels.len(),
        other.rgb,
        other.size_z,
        other.size_t,
        other.sample_bits,
    )
}

/// C, Z, T axes from fastest to slowest.
fn dimension_order(order: &str) -> Option<[usize; 3]> {
    let axes = order.strip_prefix("XY")?.as_bytes();
    let mut result = [0; 3];
    for (slot, axis) in result.iter_mut().zip(axes) {
        *slot = b"CZT".iter().position(|a| a == axis)?;
    }
    (axes.len() == 3 && result.contains(&0) && result.contains(&1) && result.contains(&2))
        .then_some(result)
}

fn describe<'a, 'd>(
    index: usize,
    image: Node<'a, 'd>,
) -> Result<(Series, Node<'a, 'd>, [usize; 3]), String> {
    let number = index + 1;
    let pixels = children(image, "Pixels")
        .next()
        .ok_or_else(|| error(format!("image {number} has no Pixels.")))?;
    let size = |key: &str| -> Result<u32, String> {
        attribute::<u32>(pixels, key)?
            .filter(|value| *value > 0)
            .ok_or_else(|| error(format!("image {number} needs a positive {key}.")))
    };
    let order = pixels
        .attribute("DimensionOrder")
        .and_then(dimension_order)
        .ok_or_else(|| error(format!("image {number} has an invalid DimensionOrder.")))?;
    let sample_bits = match pixels.attribute("Type").unwrap_or("unknown") {
        "uint8" => 8,
        "uint16" => 16,
        kind => {
            return Err(error(format!(
            "{kind} pixels are not supported yet. Only uint8 and uint16 images can be displayed."
        )))
        }
    };
    let size_c = size("SizeC")?;
    if children(pixels, "Channel").count() > MAX_CHANNELS as usize {
        return Err(error(format!(
            "at most {MAX_CHANNELS} channels are supported."
        )));
    }
    let listed: Vec<Node> = children(pixels, "Channel").collect();
    let samples = if listed.is_empty() {
        if size_c > MAX_CHANNELS {
            return Err(error(format!(
                "at most {MAX_CHANNELS} channels are supported."
            )));
        }
        vec![1; size_c as usize]
    } else {
        listed
            .iter()
            .map(|c| Ok(attribute::<u32>(*c, "SamplesPerPixel")?.unwrap_or(1)))
            .collect::<Result<Vec<_>, String>>()?
    };
    if samples.iter().map(|s| u64::from(*s)).sum::<u64>() != u64::from(size_c) {
        return Err(error(format!(
            "image {number} Channel SamplesPerPixel do not add up to SizeC={size_c}."
        )));
    }
    let rgb = samples == [3];
    if !rgb && samples.iter().any(|s| *s != 1) {
        return Err(error(format!(
            "channels with SamplesPerPixel {samples:?} are not supported. Use single-sample channels or one RGB channel."
        )));
    }
    let domain = domain(sample_bits);
    let channels = if rgb {
        [
            ("Red", "#ff0000"),
            ("Green", "#00ff00"),
            ("Blue", "#0000ff"),
        ]
        .into_iter()
        .map(|(name, color)| Channel {
            name: name.into(),
            color: color.into(),
            domain,
        })
        .collect()
    } else {
        let colors = [
            "#ff0000", "#00ff00", "#0000ff", "#ff00ff", "#00ffff", "#ffff00",
        ];
        (0..samples.len())
            .map(|c| {
                let node = listed.get(c);
                Channel {
                    name: node
                        .and_then(|n| n.attribute("Name"))
                        .filter(|s| !s.trim().is_empty())
                        .map(truncated_name)
                        .unwrap_or_else(|| format!("Channel {}", c + 1)),
                    color: node
                        .and_then(|n| n.attribute("Color"))
                        .and_then(color)
                        .unwrap_or_else(|| {
                            if samples.len() == 1 {
                                "#ffffff"
                            } else {
                                colors[c % colors.len()]
                            }
                            .into()
                        }),
                    domain,
                }
            })
            .collect()
    };
    let size_z = size("SizeZ")?;
    let size_t = size("SizeT")?;
    let logical = if rgb { 1 } else { samples.len() as u64 };
    // Checked before any plane table exists; this also bounds all index math.
    if logical
        .checked_mul(u64::from(size_z))
        .and_then(|count| count.checked_mul(u64::from(size_t)))
        .is_none_or(|count| count > MAX_PLANES)
    {
        return Err(error(format!(
            "image {number} has more than {MAX_PLANES} planes."
        )));
    }
    let series = Series {
        name: image
            .attribute("Name")
            .filter(|s| !s.trim().is_empty())
            .map(truncated_name)
            .unwrap_or_else(|| format!("Position {number}")),
        width: size("SizeX")?,
        height: size("SizeY")?,
        size_z,
        size_t,
        channels,
        rgb,
        sample_bits,
        calibration: Calibration {
            pixel_width_um: physical(pixels, "PhysicalSizeX", "µm", micrometers),
            pixel_height_um: physical(pixels, "PhysicalSizeY", "µm", micrometers),
            z_step_um: physical(pixels, "PhysicalSizeZ", "µm", micrometers),
            time_increment_seconds: physical(pixels, "TimeIncrement", "s", seconds),
        },
        planes: Vec::new(),
    };
    Ok((series, pixels, order))
}

/// OME `Color` is a signed 32-bit RGBA integer.
fn color(value: &str) -> Option<String> {
    let rgba = value.trim().parse::<i32>().ok()? as u32;
    Some(format!("#{:06x}", rgba >> 8))
}

fn physical(node: Node, key: &str, default: &str, scale: fn(&str) -> Option<f64>) -> Option<f64> {
    let value = node
        .attribute(key)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)?;
    Some(
        value
            * scale(
                node.attribute(format!("{key}Unit").as_str())
                    .unwrap_or(default),
            )?,
    )
}

fn micrometers(unit: &str) -> Option<f64> {
    Some(match unit {
        "pm" => 1e-6,
        "\u{c5}" | "\u{212b}" => 1e-4,
        "nm" => 1e-3,
        "\u{b5}m" | "\u{3bc}m" | "um" => 1.0,
        "mm" => 1e3,
        "cm" => 1e4,
        "m" => 1e6,
        _ => return None,
    })
}

fn seconds(unit: &str) -> Option<f64> {
    Some(match unit {
        "ns" => 1e-9,
        "\u{b5}s" | "\u{3bc}s" | "us" => 1e-6,
        "ms" => 1e-3,
        "s" => 1.0,
        "min" => 60.0,
        "h" => 3600.0,
        _ => return None,
    })
}

/// `TiffData` UUID text and `FileName`, when given.
fn reference<'d>(data: Node<'d, '_>) -> (Option<&'d str>, Option<&'d str>) {
    let Some(uuid) = children(data, "UUID").next() else {
        return (None, None);
    };
    (
        uuid.text().map(str::trim).filter(|s| !s.is_empty()),
        uuid.attribute("FileName").filter(|s| !s.is_empty()),
    )
}

fn checked_uuid(uuid: &str) -> Result<&str, String> {
    if uuid.len() > MAX_UUID_BYTES {
        return Err(error(format!(
            "a UUID is longer than {MAX_UUID_BYTES} bytes."
        )));
    }
    Ok(uuid)
}

fn planes<'d>(
    series: &Series,
    pixels: Node<'d, '_>,
    order: [usize; 3],
    members: &mut Members<'_, 'd>,
    assigned: &mut u64,
) -> Result<Vec<Option<PlaneRef>>, String> {
    let sizes = [series.logical_channels(), series.size_z, series.size_t];
    let total = sizes.iter().product::<u32>();
    let mut stride = [0; 3];
    let mut step = 1;
    for axis in order {
        stride[axis] = step;
        step *= sizes[axis];
    }
    let mut planes = vec![None; total as usize];
    for data in children(pixels, "TiffData") {
        let member = members.resolve(reference(data))?;
        let file = &members.files[member as usize];
        let explicit = attribute::<u32>(data, "IFD")?;
        let ifd = explicit.unwrap_or(0);
        if ifd >= file.ifds {
            return Err(error(format!(
                "TiffData IFD {ifd} is beyond the {} pages of {}.",
                file.ifds,
                file.path.display()
            )));
        }
        let mut start = 0;
        for (axis, key) in ["FirstC", "FirstZ", "FirstT"].into_iter().enumerate() {
            let first = attribute::<u32>(data, key)?.unwrap_or(0);
            if first >= sizes[axis] {
                return Err(error(format!(
                    "TiffData {key}={first} is outside {}.",
                    series.name
                )));
            }
            start += first * stride[axis];
        }
        let count = match (attribute::<u32>(data, "PlaneCount")?, explicit) {
            (Some(count), _) => {
                if u64::from(ifd) + u64::from(count) > u64::from(file.ifds)
                    || u64::from(start) + u64::from(count) > u64::from(total)
                {
                    return Err(error(format!(
                        "TiffData PlaneCount {count} exceeds the pages of {} or the planes of {}.",
                        file.path.display(),
                        series.name
                    )));
                }
                count
            }
            (None, Some(_)) => 1,
            (None, None) => (file.ifds - ifd).min(total - start),
        };
        *assigned += u64::from(count);
        if *assigned > 2 * MAX_PLANES {
            return Err(error("too many TiffData plane mappings."));
        }
        for offset in 0..count {
            let linear = start + offset;
            let [c, z, t] = [0, 1, 2].map(|axis| linear / stride[axis] % sizes[axis]);
            let plane = PlaneRef {
                member,
                ifd: ifd + offset,
            };
            let slot = &mut planes[(c + sizes[0] * (z + sizes[1] * t)) as usize];
            if slot.is_some_and(|existing| existing != plane) {
                return Err(error(format!(
                    "plane C={c} Z={z} T={t} of {} maps to two different TIFF pages.",
                    series.name
                )));
            }
            *slot = Some(plane);
        }
    }
    Ok(planes)
}

struct Member<'d> {
    path: PathBuf,
    /// UUID declared by the member's own OME-XML root.
    root: Option<String>,
    /// UUID the dataset references the member by.
    uuid: Option<&'d str>,
    ifds: u32,
}

struct Members<'o, 'd> {
    origin: &'o Origin,
    files: Vec<Member<'d>>,
    paths: HashMap<PathBuf, u16>,
    uuids: HashMap<&'d str, u16>,
    references: HashMap<(Option<&'d str>, Option<&'d str>), u16>,
    walked: u64,
}

impl<'d> Members<'_, 'd> {
    fn resolve(&mut self, (uuid, name): (Option<&'d str>, Option<&'d str>)) -> Result<u16, String> {
        if let Some(index) = self.references.get(&(uuid, name)) {
            return Ok(*index);
        }
        if let Some(uuid) = uuid {
            checked_uuid(uuid)?;
        }
        if self.references.len() >= MAX_MEMBERS {
            return Err(error(format!(
                "more than {MAX_MEMBERS} member file references."
            )));
        }
        let origin = self.origin;
        let path = match (uuid, name, &origin.opened) {
            (_, Some(name), _) => confined(&origin.directory, name)?,
            (Some(uuid), None, Some(opened)) if origin.opened_uuid.as_deref() == Some(uuid) => {
                opened.clone()
            }
            (Some(_), None, _) => {
                return Err(error(
                    "UUID-only multi-file references are not supported. Each TiffData UUID needs a FileName.",
                ))
            }
            (None, None, Some(opened)) if origin.xml_in_opened => opened.clone(),
            (None, None, _) => {
                return Err(error(
                    "TiffData without a UUID FileName must be inside an OME-TIFF.",
                ))
            }
        };
        let label = name.unwrap_or("the opened file");
        let index = match self.paths.get(&path) {
            Some(index) => *index,
            None => {
                let tiff =
                    Tiff::open(&path)?.ok_or_else(|| error(format!("{label} is not a TIFF.")))?;
                let index = self.files.len() as u16;
                let root = match tiff.root()? {
                    Root::Ome(uuid) => uuid,
                    Root::Other => None,
                    Root::Unknown => {
                        return Err(error(format!(
                            "the OME-XML root of {label} cannot be read within its description."
                        )))
                    }
                };
                self.files.push(Member {
                    root,
                    ifds: tiff.ifds(&mut self.walked)?,
                    path: path.clone(),
                    uuid: None,
                });
                self.paths.insert(path, index);
                index
            }
        };
        if let Some(uuid) = uuid {
            let file = &mut self.files[index as usize];
            if let Some(root) = file.root.as_deref().filter(|root| *root != uuid) {
                return Err(error(format!(
                    "{label} has UUID {root}, but the OME-XML references it as {uuid}."
                )));
            }
            if file.uuid.is_some_and(|previous| previous != uuid) {
                return Err(error(format!(
                    "{label} is referenced with two different UUIDs."
                )));
            }
            file.uuid = Some(uuid);
            if self
                .uuids
                .insert(uuid, index)
                .is_some_and(|other| other != index)
            {
                return Err(error(format!("UUID {uuid} names two different files.")));
            }
        }
        self.references.insert((uuid, name), index);
        Ok(index)
    }
}

/// A relative member path that stays inside the canonical `directory`.
fn confined(directory: &Path, name: &str) -> Result<PathBuf, String> {
    let raw = name.as_bytes();
    let drive = raw.len() >= 2 && raw[0].is_ascii_alphabetic() && raw[1] == b':';
    if name.starts_with(['/', '\\'])
        || drive
        || name.split(['/', '\\']).any(|part| part == "..")
        || Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(error(format!(
            "member file {name:?} must be a relative path inside the dataset folder."
        )));
    }
    let path = std::fs::canonicalize(directory.join(name))
        .map_err(|e| error(format!("member file {name:?} cannot be opened: {e}")))?;
    if path.as_os_str().len() > MAX_PATH_BYTES {
        return Err(error(format!(
            "member file {name:?} has a path that is too long."
        )));
    }
    if !path.starts_with(directory) || !path.is_file() {
        return Err(error(format!(
            "member file {name:?} resolves outside the dataset folder."
        )));
    }
    Ok(path)
}

/// The root element's local name and `UUID` attribute at the start of XML text.
fn root_start(text: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    fn after<'a>(text: &'a [u8], end: &[u8]) -> Option<&'a [u8]> {
        let at = text.windows(end.len()).position(|w| w == end)?;
        Some(&text[at + end.len()..])
    }
    let mut rest = text.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(text);
    loop {
        rest = rest.trim_ascii_start();
        if let Some(tail) = rest.strip_prefix(b"<?") {
            rest = after(tail, b"?>")?;
        } else if let Some(tail) = rest.strip_prefix(b"<!--") {
            rest = after(tail, b"-->")?;
        } else {
            break;
        }
    }
    rest = rest.strip_prefix(b"<")?;
    let end = rest
        .iter()
        .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))?;
    let local = rest[..end].rsplit(|b| *b == b':').next()?;
    rest = &rest[end..];
    let mut uuid = None;
    loop {
        rest = rest.trim_ascii_start();
        if rest.starts_with(b">") || rest.starts_with(b"/>") {
            return Some((local, uuid));
        }
        let end = rest
            .iter()
            .position(|b| b.is_ascii_whitespace() || *b == b'=')?;
        let key = &rest[..end];
        rest = rest[end..]
            .trim_ascii_start()
            .strip_prefix(b"=")?
            .trim_ascii_start();
        let quote = *rest.first().filter(|q| matches!(q, b'"' | b'\''))?;
        let close = rest[1..].iter().position(|b| *b == quote)? + 1;
        if key == b"UUID" {
            uuid = Some(&rest[1..close]);
        }
        rest = &rest[close + 1..];
    }
}

enum Root {
    /// An OME root with its `UUID` attribute, if any.
    Ome(Option<String>),
    /// Not XML, or XML with another root.
    Other,
    /// XML whose prolog or root start tag does not end within the probe.
    Unknown,
}

/// A byte range of a TIFF read in windows of at most `HEADER_BYTES`.
struct Text<'a> {
    tiff: &'a Tiff,
    at: u64,
    end: u64,
}

impl Text<'_> {
    fn window(&self, len: u64) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0; len.min(self.end.saturating_sub(self.at)) as usize];
        self.tiff.read(self.at, &mut bytes)?;
        Ok(bytes)
    }

    /// Advances past `pattern`; false when the range ends first.
    fn skip_past(&mut self, pattern: &[u8]) -> Result<bool, String> {
        loop {
            let window = self.window(HEADER_BYTES)?;
            if let Some(at) = window.windows(pattern.len()).position(|w| w == pattern) {
                self.at += (at + pattern.len()) as u64;
                return Ok(true);
            }
            if window.len() < HEADER_BYTES as usize {
                return Ok(false);
            }
            // Keep an overlap so a pattern across windows is still found.
            self.at += (window.len() - (pattern.len() - 1)) as u64;
        }
    }
}

struct Tiff {
    file: File,
    len: u64,
    little: bool,
    big: bool,
    first: u64,
}

impl Tiff {
    /// `None` when `path` is not a TIFF.
    fn open(path: &Path) -> Result<Option<Self>, String> {
        let file =
            File::open(path).map_err(|e| error(format!("cannot open {}: {e}", path.display())))?;
        let len = file.metadata().map_err(error)?.len();
        let mut tiff = Self {
            file,
            len,
            little: true,
            big: false,
            first: 0,
        };
        let mut header = [0; 16];
        if len < 16 || tiff.read(0, &mut header).is_err() {
            return Ok(None);
        }
        let Ok((little, big)) = tiff_ifd::header(&header) else {
            return Ok(None);
        };
        tiff.little = little;
        tiff.big = big;
        tiff.first = number(if big { &header[8..16] } else { &header[4..8] }, little);
        Ok(Some(tiff))
    }

    fn read(&self, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(error("a TIFF directory points outside its file."));
        }
        (&self.file)
            .seek(SeekFrom::Start(offset))
            .and_then(|_| (&self.file).read_exact(bytes))
            .map_err(error)
    }

    fn number(&self, offset: u64, size: usize) -> Result<u64, String> {
        let mut bytes = [0; 8];
        self.read(offset, &mut bytes[..size])?;
        Ok(number(&bytes[..size], self.little))
    }

    fn word(&self) -> usize {
        if self.big {
            8
        } else {
            4
        }
    }

    fn entry_size(&self) -> u64 {
        if self.big {
            20
        } else {
            12
        }
    }

    /// Entry count and the offset of the first entry.
    fn entries(&self, ifd: u64) -> Result<(u64, u64), String> {
        let size = if self.big { 8 } else { 2 };
        let count = self.number(ifd, size)?;
        if count > MAX_IFD_ENTRIES {
            return Err(error("a TIFF directory has too many entries."));
        }
        Ok((count, ifd + size as u64))
    }

    /// Main-chain IFD count; `walked` bounds the total over all members.
    fn ifds(&self, walked: &mut u64) -> Result<u32, String> {
        // Brent's cycle detection keeps loop checks O(1) in memory.
        let (mut offset, mut saved, mut count) = (self.first, self.first, 0_u32);
        let (mut power, mut steps) = (1_u32, 0_u32);
        while offset != 0 {
            *walked += 1;
            if *walked > MAX_IFDS {
                return Err(error(format!(
                    "member files contain more than {MAX_IFDS} TIFF pages."
                )));
            }
            count += 1;
            let (entries, start) = self.entries(offset)?;
            offset = self.number(start + entries * self.entry_size(), self.word())?;
            if offset == saved {
                return Err(error("a TIFF page chain loops."));
            }
            steps += 1;
            if steps == power {
                (saved, power, steps) = (offset, power * 2, 0);
            }
        }
        Ok(count)
    }

    /// Offset and length of the first IFD's ImageDescription.
    fn description(&self) -> Result<Option<(u64, u64)>, String> {
        let (entries, start) = self.entries(self.first)?;
        let word = self.word();
        for index in 0..entries {
            let at = start + index * self.entry_size();
            let mut raw = [0; 20];
            self.read(at, &mut raw[..self.entry_size() as usize])?;
            if number(&raw[..2], self.little) != 270
                || !matches!(number(&raw[2..4], self.little), 1 | 2 | 7)
            {
                continue;
            }
            let len = number(&raw[4..4 + word], self.little);
            let offset = if len <= word as u64 {
                at + 4 + word as u64
            } else {
                number(&raw[4 + word..4 + 2 * word], self.little)
            };
            return Ok(Some((offset, len)));
        }
        Ok(None)
    }

    /// The first description's root element, read in bounded windows so that
    /// a long prolog never needs the whole XML in memory.
    fn root(&self) -> Result<Root, String> {
        let Some((offset, len)) = self.description()? else {
            return Ok(Root::Other);
        };
        let mut text = Text {
            tiff: self,
            at: offset,
            end: offset + len.min(MAX_XML_BYTES),
        };
        if text.window(3)? == b"\xEF\xBB\xBF" {
            text.at += 3;
        }
        loop {
            let window = text.window(HEADER_BYTES)?;
            let blank = window.len() - window.trim_ascii_start().len();
            if blank > 0 {
                text.at += blank as u64;
                continue;
            }
            let terminator: &[u8] = if window.starts_with(b"<?") {
                b"?>"
            } else if window.starts_with(b"<!--") {
                b"-->"
            } else if window.starts_with(b"<") {
                return Ok(match root_start(&window) {
                    Some((b"OME", uuid)) => Root::Ome(
                        uuid.map(|uuid| {
                            std::str::from_utf8(uuid)
                                .map_err(|_| error("a root UUID is not UTF-8."))
                                .and_then(checked_uuid)
                                .map(str::to_owned)
                        })
                        .transpose()?,
                    ),
                    Some(_) => Root::Other,
                    None => Root::Unknown,
                });
            } else if window.is_empty() && len > MAX_XML_BYTES {
                // Whitespace up to the XML limit; the rest is never read.
                return Ok(Root::Unknown);
            } else {
                // Empty or not XML.
                return Ok(Root::Other);
            };
            text.at += 2;
            if !text.skip_past(terminator)? {
                return Ok(Root::Unknown);
            }
        }
    }

    /// Whether the first description mentions an OME element anywhere in its
    /// first 32 MiB, including prefixed names.
    fn mentions_ome(&self) -> Result<bool, String> {
        let Some((offset, len)) = self.description()? else {
            return Ok(false);
        };
        for pattern in [&b"<OME"[..], b":OME"] {
            let mut text = Text {
                tiff: self,
                at: offset,
                end: offset + len.min(MAX_XML_BYTES),
            };
            if text.skip_past(pattern)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::tests::temporary;
    use crate::memory::MIN_LIMIT_BYTES;

    const NS: &str = "http://www.openmicroscopy.org/Schemas/OME/2016-06";

    /// 1×1 8-bit pages; the first carries `description`.
    fn write_tiff(path: &Path, big: bool, little: bool, pages: usize, description: Option<&str>) {
        let word = if big { 8 } else { 4 };
        let push = |out: &mut Vec<u8>, value: u64, size: usize| {
            let raw = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            out.extend_from_slice(if little {
                &raw[..size]
            } else {
                &raw[8 - size..]
            });
        };
        let mut out = Vec::new();
        out.extend_from_slice(if little { b"II" } else { b"MM" });
        push(&mut out, if big { 43 } else { 42 }, 2);
        if big {
            push(&mut out, 8, 2);
            push(&mut out, 0, 2);
        }
        let mut link = out.len();
        push(&mut out, 0, word);
        for page in 0..pages {
            let text = description
                .filter(|_| page == 0)
                .map(|text| [text.as_bytes(), &b"\0"[..]].concat());
            let text_at = out.len() as u64;
            if let Some(text) = &text {
                assert!(text.len() > word);
                out.extend_from_slice(text);
            }
            let pixel_at = out.len() as u64;
            out.push(page as u8);
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let mut next = Vec::new();
            push(&mut next, out.len() as u64, word);
            out[link..link + word].copy_from_slice(&next);
            let offset = if big { 16 } else { 4 };
            let mut tags = vec![(256, 3, 1, 1), (257, 3, 1, 1), (258, 3, 1, 8)];
            if let Some(text) = &text {
                tags.push((270, 2, text.len() as u64, text_at));
            }
            tags.extend([
                (273, offset, 1, pixel_at),
                (277, 3, 1, 1),
                (278, 3, 1, 1),
                (279, 3, 1, 1),
            ]);
            push(&mut out, tags.len() as u64, if big { 8 } else { 2 });
            for (tag, kind, count, value) in tags {
                push(&mut out, tag, 2);
                push(&mut out, kind, 2);
                push(&mut out, count, word);
                let size = match kind {
                    3 => 2,
                    4 => 4,
                    _ => word,
                };
                push(&mut out, value, size);
                out.resize(out.len() + word - size, 0);
            }
            link = out.len();
            push(&mut out, 0, word);
        }
        std::fs::write(path, out).unwrap();
    }

    fn ome(uuid: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><OME xmlns="{NS}" UUID="{uuid}">{body}</OME>"#
        )
    }

    /// One image with C/Z/T sizes, extra Pixels attributes and children.
    fn image(
        name: &str,
        order: &str,
        kind: &str,
        czt: [u32; 3],
        extra: &str,
        body: &str,
    ) -> String {
        let [c, z, t] = czt;
        format!(
            r#"<Image ID="Image:{name}" Name="{name}"><Pixels ID="Pixels:{name}" DimensionOrder="{order}" Type="{kind}" SizeX="4" SizeY="3" SizeC="{c}" SizeZ="{z}" SizeT="{t}" {extra}>{body}</Pixels></Image>"#
        )
    }

    fn budget(dir: &Path) -> Arc<Budget> {
        Budget::new(dir.join("budget")).unwrap()
    }

    /// Writes `data.ome.tif` with `pages` IFDs and opens it.
    fn opened(dir: &Path, pages: usize, xml: &str) -> Result<Dataset, String> {
        let path = dir.join("data.ome.tif");
        write_tiff(&path, false, true, pages, Some(xml));
        open(&path, &budget(dir)).map(|(dataset, _)| dataset)
    }

    fn page(series: &Series, c: u32, z: u32, t: u32) -> Option<(u16, u32)> {
        series.plane(c, z, t).map(|plane| (plane.member, plane.ifd))
    }

    fn expect_error(result: Result<Dataset, String>, text: &str) {
        let message = result.unwrap_err();
        assert!(
            message.starts_with("OME-TIFF: ") && message.contains(text),
            "{message:?} lacks {text:?}"
        );
    }

    #[test]
    fn detects_ome_descriptions_in_all_tiff_layouts_and_companions() {
        let dir = temporary();
        let xml = ome("urn:uuid:a", "");
        for (big, little) in [(false, true), (false, false), (true, true), (true, false)] {
            let path = dir.join(format!("{big}-{little}.tif"));
            write_tiff(&path, big, little, 2, Some(&xml));
            assert!(detect(&path).unwrap());
            let prefixed =
                format!("<?xml version=\"1.0\"?><!-- x --><ome:OME xmlns:ome=\"{NS}\" UUID='u'/>");
            write_tiff(&path, big, little, 1, Some(&prefixed));
            assert!(detect(&path).unwrap());
            write_tiff(&path, big, little, 1, Some("ImageJ=1.54\nimages=1\n"));
            assert!(!detect(&path).unwrap());
            write_tiff(&path, big, little, 1, None);
            assert!(!detect(&path).unwrap());
        }
        let other = dir.join("plain.txt");
        std::fs::write(&other, "<OME/>").unwrap();
        assert!(!detect(&other).unwrap());
        assert!(detect(&dir.join("Set.Companion.OME")).unwrap());
    }

    #[test]
    fn ifd_only_tiffdata_in_classic_and_bigtiff_of_both_byte_orders() {
        let dir = temporary();
        let body = (0..4)
            .map(|i| {
                format!(
                    r#"<TiffData IFD="{}" FirstC="{}" FirstZ="{}" PlaneCount="1"/>"#,
                    3 - i,
                    i % 2,
                    i / 2
                )
            })
            .collect::<String>();
        let xml = ome(
            "urn:uuid:a",
            &image(
                "A",
                "XYCZT",
                "uint16",
                [2, 2, 1],
                "",
                &format!(
                    r#"<Channel Name="DAPI" Color="65535"/><Channel Name=" " Color="x"/>{body}"#
                ),
            ),
        );
        for (big, little) in [(false, true), (false, false), (true, true), (true, false)] {
            let path = dir.join(format!("{big}-{little}.ome.tif"));
            write_tiff(&path, big, little, 4, Some(&xml));
            let (dataset, reservation) = open(&path, &budget(&dir)).unwrap();
            assert!(reservation.bytes() > 0);
            assert_eq!(dataset.members, [std::fs::canonicalize(&path).unwrap()]);
            let series = &dataset.series[0];
            assert_eq!(
                (series.width, series.height, series.sample_bits),
                (4, 3, 16)
            );
            assert_eq!(page(series, 0, 0, 0), Some((0, 3)));
            assert_eq!(page(series, 1, 0, 0), Some((0, 2)));
            assert_eq!(page(series, 0, 1, 0), Some((0, 1)));
            assert_eq!(page(series, 1, 1, 0), Some((0, 0)));
            assert_eq!(page(series, 2, 0, 0), None);
            assert_eq!(series.channels[0].name, "DAPI");
            assert_eq!(series.channels[0].color, "#0000ff");
            assert_eq!(series.channels[0].domain, [0, 65535]);
            assert_eq!(series.channels[1].name, "Channel 2");
            assert_eq!(series.channels[1].color, "#00ff00");
        }
    }

    fn linear(order: &str, sizes: [u32; 3], czt: [u32; 3]) -> u32 {
        order[2..].bytes().rev().fold(0, |value, axis| {
            let axis = b"CZT".iter().position(|a| *a == axis).unwrap();
            value * sizes[axis] + czt[axis]
        })
    }

    #[test]
    fn first_and_plane_count_follow_every_dimension_order() {
        let dir = temporary();
        let sizes = [2, 3, 2];
        for order in ["XYZCT", "XYZTC", "XYCZT", "XYCTZ", "XYTZC", "XYTCZ"] {
            // The second block starts at linear plane 5, expressed as FirstC/Z/T.
            let mut first = [0; 3];
            for c in 0..2 {
                for z in 0..3 {
                    for t in 0..2 {
                        if linear(order, sizes, [c, z, t]) == 5 {
                            first = [c, z, t];
                        }
                    }
                }
            }
            let [c, z, t] = first;
            for body in [
                r#"<TiffData IFD="0" PlaneCount="12"/>"#.to_string(),
                format!(
                    r#"<TiffData PlaneCount="5"/><TiffData IFD="5" FirstC="{c}" FirstZ="{z}" FirstT="{t}" PlaneCount="7"/>"#
                ),
            ] {
                let xml = ome("urn:uuid:a", &image("A", order, "uint8", sizes, "", &body));
                let dataset = opened(&dir, 12, &xml).unwrap();
                let series = &dataset.series[0];
                for c in 0..2 {
                    for z in 0..3 {
                        for t in 0..2 {
                            assert_eq!(
                                page(series, c, z, t),
                                Some((0, linear(order, sizes, [c, z, t]))),
                                "{order} {body}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn attribute_less_tiffdata_maps_the_smaller_of_pages_and_planes() {
        let dir = temporary();
        let xml = ome(
            "urn:uuid:a",
            &image("A", "XYZCT", "uint8", [1, 4, 1], "", "<TiffData/>"),
        );
        // Fewer pages than planes leaves the remaining planes unmapped.
        let series = &opened(&dir, 2, &xml).unwrap().series[0];
        assert_eq!(
            (0..4).map(|z| page(series, 0, z, 0)).collect::<Vec<_>>(),
            [Some((0, 0)), Some((0, 1)), None, None]
        );
        // Extraneous pages are ignored.
        let series = &opened(&dir, 6, &xml).unwrap().series[0];
        assert_eq!(page(series, 0, 3, 0), Some((0, 3)));
        // IFD alone maps one plane; FirstZ alone maps from page 0.
        let xml = ome(
            "urn:uuid:a",
            &image(
                "A",
                "XYZCT",
                "uint8",
                [1, 4, 1],
                "",
                r#"<TiffData IFD="5"/><TiffData FirstZ="2"/>"#,
            ),
        );
        let series = &opened(&dir, 6, &xml).unwrap().series[0];
        assert_eq!(
            (0..4).map(|z| page(series, 0, z, 0)).collect::<Vec<_>>(),
            [Some((0, 5)), None, Some((0, 0)), Some((0, 1))]
        );
    }

    #[test]
    fn unmapped_planes_are_allowed_but_contradictions_reject() {
        let dir = temporary();
        let xml = |body: &str| {
            ome(
                "urn:uuid:a",
                &image("A", "XYZCT", "uint8", [1, 3, 1], "", body),
            )
        };
        let series = &opened(&dir, 3, &xml(r#"<TiffData IFD="0"/><TiffData IFD="2" FirstZ="2"/><TiffData IFD="0" FirstZ="0"/>"#))
            .unwrap()
            .series[0];
        assert_eq!(page(series, 0, 1, 0), None);
        assert_eq!(page(series, 0, 2, 0), Some((0, 2)));
        for (body, message) in [
            (
                r#"<TiffData IFD="0"/><TiffData IFD="1"/>"#,
                "maps to two different",
            ),
            (r#"<TiffData IFD="3"/>"#, "beyond the 3 pages"),
            (
                r#"<TiffData IFD="2" PlaneCount="2"/>"#,
                "PlaneCount 2 exceeds",
            ),
            (
                r#"<TiffData FirstZ="1" PlaneCount="3"/>"#,
                "PlaneCount 3 exceeds",
            ),
            (r#"<TiffData FirstZ="3"/>"#, "FirstZ=3 is outside"),
            (r#"<TiffData IFD="-1"/>"#, "invalid IFD"),
            ("", "no image planes"),
        ] {
            expect_error(opened(&dir, 3, &xml(body)), message);
        }
        // A page chain that loops back is rejected while counting pages.
        let path = dir.join("data.ome.tif");
        write_tiff(&path, false, true, 3, Some(&xml("<TiffData/>")));
        let tiff = Tiff::open(&path).unwrap().unwrap();
        let link = |ifd: u64| {
            let (entries, start) = tiff.entries(ifd).unwrap();
            start + entries * 12
        };
        let second = tiff.number(link(tiff.first), 4).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let at = link(second) as usize;
        bytes[at..at + 4].copy_from_slice(&(tiff.first as u32).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        expect_error(
            open(&path, &budget(&dir)).map(|(d, _)| d),
            "page chain loops",
        );
    }

    #[test]
    fn uuid_file_references_resolve_and_verify_members() {
        let dir = temporary();
        let body = |b_uuid: &str| {
            image(
                "A",
                "XYZCT",
                "uint8",
                [1, 4, 1],
                "",
                &format!(
                    r#"<TiffData PlaneCount="2"><UUID FileName="a.ome.tif">urn:uuid:a</UUID></TiffData><TiffData FirstZ="2"><UUID FileName="b.ome.tif">{b_uuid}</UUID></TiffData>"#
                ),
            )
        };
        let a = dir.join("a.ome.tif");
        let b = dir.join("b.ome.tif");
        write_tiff(
            &a,
            false,
            true,
            2,
            Some(&ome("urn:uuid:a", &body("urn:uuid:b"))),
        );
        write_tiff(
            &b,
            true,
            false,
            2,
            Some(&ome("urn:uuid:b", &body("urn:uuid:b"))),
        );
        for path in [&a, &b] {
            let (dataset, _) = open(path, &budget(&dir)).unwrap();
            assert_eq!(
                dataset.members,
                [a.canonicalize().unwrap(), b.canonicalize().unwrap()]
            );
            let series = &dataset.series[0];
            assert_eq!(
                (0..4).map(|z| page(series, 0, z, 0)).collect::<Vec<_>>(),
                [Some((0, 0)), Some((0, 1)), Some((1, 0)), Some((1, 1))]
            );
        }
        // The member's own root UUID must match its reference.
        write_tiff(
            &a,
            false,
            true,
            2,
            Some(&ome("urn:uuid:a", &body("urn:uuid:other"))),
        );
        expect_error(
            open(&a, &budget(&dir)).map(|(d, _)| d),
            "has UUID urn:uuid:b, but the OME-XML references it as urn:uuid:other",
        );
        // Members without OME-XML still need consistent UUID references.
        write_tiff(&dir.join("c.tif"), false, true, 2, None);
        write_tiff(&dir.join("d.tif"), false, true, 2, None);
        let reference = |file: &str, uuid: &str, first: u32| {
            format!(
                r#"<TiffData FirstZ="{first}" PlaneCount="1"><UUID FileName="{file}">{uuid}</UUID></TiffData>"#
            )
        };
        let xml = |body: String| {
            ome(
                "urn:uuid:a",
                &image("A", "XYZCT", "uint8", [1, 2, 1], "", &body),
            )
        };
        let both = [
            reference("c.tif", "urn:uuid:c", 0),
            reference("d.tif", "urn:uuid:d", 1),
        ]
        .concat();
        assert_eq!(opened(&dir, 1, &xml(both)).unwrap().members.len(), 2);
        expect_error(
            opened(
                &dir,
                1,
                &xml([
                    reference("c.tif", "urn:uuid:c", 0),
                    reference("c.tif", "urn:uuid:x", 1),
                ]
                .concat()),
            ),
            "referenced with two different UUIDs",
        );
        expect_error(
            opened(
                &dir,
                1,
                &xml([
                    reference("c.tif", "urn:uuid:c", 0),
                    reference("d.tif", "urn:uuid:c", 1),
                ]
                .concat()),
            ),
            "names two different files",
        );
        std::fs::write(dir.join("e.tif"), "not a tiff").unwrap();
        expect_error(
            opened(&dir, 1, &xml(reference("e.tif", "urn:uuid:e", 0))),
            "is not a TIFF",
        );
        expect_error(
            opened(&dir, 1, &xml(reference("missing.tif", "urn:uuid:m", 0))),
            "cannot be opened",
        );
    }

    #[test]
    fn uuid_only_references_resolve_only_to_the_opened_file() {
        let dir = temporary();
        let xml = |uuid: &str| {
            ome(
                "urn:uuid:self",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 2, 1],
                    "",
                    &format!("<TiffData><UUID>{uuid}</UUID></TiffData>"),
                ),
            )
        };
        let dataset = opened(&dir, 2, &xml("urn:uuid:self")).unwrap();
        assert_eq!(page(&dataset.series[0], 0, 1, 0), Some((0, 1)));
        expect_error(
            opened(&dir, 2, &xml("urn:uuid:other")),
            "UUID-only multi-file references are not supported",
        );
    }

    #[test]
    fn binary_only_follows_its_companion_metadata_file() {
        let dir = temporary();
        let data = dir.join("data.ome.tif");
        let companion = dir.join("set.companion.ome");
        let binary = |file: &str, uuid: &str| {
            ome(
                "urn:uuid:data",
                &format!(r#"<BinaryOnly MetadataFile="{file}" UUID="{uuid}"/>"#),
            )
        };
        let metadata = |reference: &str| {
            ome(
                "urn:uuid:meta",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 3, 1],
                    "",
                    &format!("<TiffData>{reference}</TiffData>"),
                ),
            )
        };
        write_tiff(
            &data,
            true,
            true,
            3,
            Some(&binary("set.companion.ome", "urn:uuid:meta")),
        );
        std::fs::write(
            &companion,
            metadata(r#"<UUID FileName="data.ome.tif">urn:uuid:data</UUID>"#),
        )
        .unwrap();
        for path in [&data, &companion] {
            let (dataset, _) = open(path, &budget(&dir)).unwrap();
            assert_eq!(dataset.members, [data.canonicalize().unwrap()]);
            assert_eq!(page(&dataset.series[0], 0, 2, 0), Some((0, 2)));
        }
        // A UUID-only reference names the opened TIFF, not the companion.
        std::fs::write(&companion, metadata("<UUID>urn:uuid:data</UUID>")).unwrap();
        assert!(open(&data, &budget(&dir)).is_ok());
        expect_error(open(&companion, &budget(&dir)).map(|(d, _)| d), "UUID-only");
        std::fs::write(&companion, metadata("")).unwrap();
        expect_error(
            open(&companion, &budget(&dir)).map(|(d, _)| d),
            "must be inside an OME-TIFF",
        );
        expect_error(
            open(&data, &budget(&dir)).map(|(d, _)| d),
            "must be inside an OME-TIFF",
        );
        write_tiff(
            &data,
            true,
            true,
            3,
            Some(&binary("set.companion.ome", "urn:uuid:x")),
        );
        expect_error(
            open(&data, &budget(&dir)).map(|(d, _)| d),
            "does not match its reference",
        );
        write_tiff(
            &data,
            true,
            true,
            3,
            Some(&binary("../set.companion.ome", "urn:uuid:meta")),
        );
        expect_error(open(&data, &budget(&dir)).map(|(d, _)| d), "relative path");
        std::fs::write(&companion, binary("data.ome.tif", "urn:uuid:meta")).unwrap();
        expect_error(
            open(&companion, &budget(&dir)).map(|(d, _)| d),
            "BinaryOnly must be inside",
        );
        // Bio-Formats also points BinaryOnly members at the OME-TIFF holding
        // the full metadata in its description.
        let full = dir.join("full.ome.tif");
        let reference = |file: &str, uuid: &str| {
            format!(r#"<TiffData PlaneCount="1"><UUID FileName="{file}">{uuid}</UUID></TiffData>"#)
        };
        let both = ome(
            "urn:uuid:full",
            &image(
                "A",
                "XYZCT",
                "uint8",
                [1, 2, 1],
                "",
                &[
                    reference("full.ome.tif", "urn:uuid:full"),
                    reference("data.ome.tif", "urn:uuid:data")
                        .replace("<TiffData ", r#"<TiffData FirstZ="1" "#),
                ]
                .concat(),
            ),
        );
        write_tiff(&full, false, true, 1, Some(&both));
        write_tiff(
            &data,
            true,
            true,
            1,
            Some(&binary("full.ome.tif", "urn:uuid:full")),
        );
        let (dataset, _) = open(&data, &budget(&dir)).unwrap();
        assert_eq!(page(&dataset.series[0], 0, 1, 0), Some((1, 0)));
        assert_eq!(dataset.members[0], full.canonicalize().unwrap());
        write_tiff(&full, false, true, 1, None);
        expect_error(
            open(&data, &budget(&dir)).map(|(d, _)| d),
            "has no OME-XML description",
        );
    }

    #[test]
    fn member_paths_stay_inside_the_dataset_folder() {
        let root = temporary();
        let dir = root.join("set");
        std::fs::create_dir(&dir).unwrap();
        let outside = root.join("outside.tif");
        write_tiff(&outside, false, true, 1, None);
        let absolute = outside.to_str().unwrap().to_string();
        for name in [
            "../outside.tif",
            "sub/../../outside.tif",
            "sub\\..\\..\\outside.tif",
            absolute.as_str(),
            "C:\\data\\outside.tif",
            "C:outside.tif",
            "\\\\server\\share\\outside.tif",
            "\\\\?\\C:\\outside.tif",
            "",
        ] {
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 1, 1],
                    "",
                    &format!(r#"<TiffData><UUID FileName="{name}">urn:uuid:o</UUID></TiffData>"#),
                ),
            );
            let result = opened(&dir, 1, &xml);
            if name.is_empty() {
                // An empty FileName is a UUID-only reference.
                expect_error(result, "UUID-only");
            } else {
                expect_error(result, "must be a relative path");
            }
        }
        std::fs::create_dir(dir.join("sub")).unwrap();
        write_tiff(&dir.join("sub/b.tif"), false, true, 1, None);
        let xml = ome(
            "urn:uuid:a",
            &image(
                "A",
                "XYZCT",
                "uint8",
                [1, 1, 1],
                "",
                r#"<TiffData><UUID FileName="./sub/b.tif">urn:uuid:b</UUID></TiffData>"#,
            ),
        );
        assert_eq!(
            opened(&dir, 1, &xml).unwrap().members,
            [dir.join("sub/b.tif").canonicalize().unwrap()]
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.join("link.tif")).unwrap();
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 1, 1],
                    "",
                    r#"<TiffData><UUID FileName="link.tif">urn:uuid:o</UUID></TiffData>"#,
                ),
            );
            expect_error(opened(&dir, 1, &xml), "resolves outside the dataset folder");
        }
    }

    #[test]
    fn markup_is_bounded_before_parsing() {
        let dir = temporary();
        let budget = budget(&dir);
        let parsed = |text: &str| parse(text, &budget).map(|_| ());
        let dtd = format!("<?xml version=\"1.0\"?><!DOCTYPE OME [<!ENTITY x \"y\">]><OME xmlns=\"{NS}\">&x;</OME>");
        assert!(parsed(&dtd).unwrap_err().contains("DTD"));
        expect_error(opened(&dir, 1, &dtd), "DTD");
        // Text between elements adds nodes without raw '<'.
        let nodes = format!("<OME>{}</OME>", "<a/>x".repeat(1_500_000));
        assert!(parsed(&nodes).unwrap_err().contains("nodes"));
        let comments = format!("<OME>{}</OME>", "<!---->".repeat(MAX_MARKUP));
        assert!(comments.len() < MAX_XML_BYTES as usize);
        assert!(parsed(&comments)
            .unwrap_err()
            .contains("elements or attributes"));
        let attributes = format!("<OME a='{}'/>", "=".repeat(MAX_MARKUP));
        assert!(parsed(&attributes)
            .unwrap_err()
            .contains("elements or attributes"));
        assert_eq!(budget.status().unwrap().used_bytes, 0);
        // The description length is checked before reading it.
        let path = dir.join("large.ome.tif");
        write_tiff(&path, false, true, 1, Some(&ome("u", "")));
        let mut bytes = std::fs::read(&path).unwrap();
        let entry = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 2 + 3 * 12;
        bytes[entry + 4..entry + 8].copy_from_slice(&(33_u32 << 20).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        expect_error(open(&path, &budget).map(|(d, _)| d), "32 MiB");
        let companion = dir.join("large.companion.ome");
        File::create(&companion)
            .unwrap()
            .set_len(MAX_XML_BYTES + 1)
            .unwrap();
        expect_error(open(&companion, &budget).map(|(d, _)| d), "32 MiB");
    }

    #[test]
    fn namespace_declarations_are_capped_on_raw_text() {
        let dir = temporary();
        let budget = budget(&dir);
        let parsed = |text: &str| parse(text, &budget).map(|_| ());
        let prefixed = |count: usize| {
            (0..count)
                .map(|i| format!(" xmlns:p{i}=\"urn:p{i}\""))
                .collect::<String>()
        };
        let exact = format!("<OME xmlns=\"{NS}\"{}/>", prefixed(63));
        assert!(parsed(&exact).is_ok());
        assert!(parsed(&exact.replace("/>", "><!-- xmlns --></OME>"))
            .unwrap_err()
            .contains("namespaces"));
        let text = format!(
            "<OME xmlns=\"{NS}\"><!--{}-->xmlns</OME>",
            "xmlns".repeat(62)
        );
        assert!(parsed(&text).is_ok());
        assert!(parsed(&text.replace("</OME>", "xmlns</OME>"))
            .unwrap_err()
            .contains("namespaces"));
        // Whitespace around '=' still counts as declarations.
        let spaced = ["xmlns = ''", "xmlns\t=\t''", "xmlns\r=\r''", "xmlns\n=\n''"]
            .map(|declaration| format!("<a {declaration}/>"))
            .concat();
        let redeclared = format!("<OME{}>{spaced}</OME>", prefixed(63));
        assert!(parsed(&redeclared).unwrap_err().contains("namespaces"));
        let children = (0..65)
            .map(|i| format!("<p{}:a xmlns:p{}='urn:x'/>", i % 4, i % 4))
            .collect::<String>();
        let many = format!("<OME{}>{children}</OME>", prefixed(4));
        assert!(parsed(&many).unwrap_err().contains("namespaces"));
    }

    #[test]
    fn parser_memory_is_admitted_by_the_budget() {
        let dir = temporary();
        let budget = budget(&dir);
        budget.set_limit(MIN_LIMIT_BYTES).unwrap();
        // 1.5 M raw '<' reserve more than the 256 MiB limit before parsing.
        let text = format!("<OME xmlns=\"{NS}\">{}</OME>", "<a/>".repeat(1_500_000));
        let message = parse(&text, &budget).map(|_| ()).unwrap_err();
        assert!(message.contains("memory budget"), "{message}");
        assert_eq!(budget.status().unwrap().used_bytes, 0);
    }

    #[test]
    fn plane_counts_are_checked_before_any_table_or_index() {
        let dir = temporary();
        // Both products wrap in 32 bits; neither may open or index.
        for (c, z, t, channels) in [
            (32, 134_234_113, 4_294_443_040_u64, ""),
            (4, 2_147_418_113, 2_147_549_185, ""),
            (1, 1 << 20, 2, r#"<Channel SamplesPerPixel="1"/>"#),
        ] {
            let xml = ome(
                "urn:uuid:a",
                &format!(
                    r#"<Image ID="Image:0" Name="A"><Pixels ID="Pixels:0" DimensionOrder="XYCZT" Type="uint8" SizeX="1" SizeY="1" SizeC="{c}" SizeZ="{z}" SizeT="{t}">{channels}<TiffData IFD="0" PlaneCount="1"/></Pixels></Image>"#
                ),
            );
            expect_error(opened(&dir, 1, &xml), "planes");
        }
        // A wrapped series after a valid first one is skipped, never indexed.
        let xml = ome(
            "urn:uuid:a",
            &format!(
                "{}{}",
                image("A", "XYZCT", "uint8", [1, 1, 1], "", "<TiffData/>"),
                image(
                    "B",
                    "XYZCT",
                    "uint8",
                    [4, 2_147_418_113, 2_147_549_185],
                    "",
                    "<TiffData/>"
                )
            ),
        );
        let dataset = opened(&dir, 1, &xml).unwrap();
        assert_eq!((dataset.series.len(), dataset.skipped), (1, 1));
        assert_eq!(dataset.series[0].plane(0, 0, 1), None);
        assert_eq!(dataset.series[0].plane(0, u32::MAX, u32::MAX), None);
    }

    #[test]
    fn root_probes_read_past_long_prologs_and_never_guess() {
        let dir = temporary();
        let comment = format!("<!-- {} -->", "x".repeat(70 * 1024));
        let body = image("A", "XYZCT", "uint8", [1, 2, 1], "", "<TiffData/>");
        let late = format!(
            r#"<?xml version="1.0"?>{comment}<OME xmlns="{NS}" UUID="urn:uuid:a">{body}</OME>"#
        );
        let path = dir.join("late.tif");
        write_tiff(&path, false, true, 2, Some(&late));
        assert!(detect(&path).unwrap());
        let dataset = open(&path, &budget(&dir)).unwrap().0;
        assert_eq!(page(&dataset.series[0], 0, 1, 0), Some((0, 1)));
        // OME-XML the probe cannot place still takes the OME path and fails there.
        for text in [
            format!("<!-- unterminated {comment}<OME xmlns=\"{NS}\"/>"),
            format!("<!DOCTYPE OME><OME xmlns=\"{NS}\"/>"),
            format!(
                "<OME xmlns=\"{NS}\" {} UUID=\"u\">",
                "a=\"b\" ".repeat(20_000)
            ),
        ] {
            write_tiff(&path, false, true, 1, Some(&text));
            assert!(detect(&path).unwrap());
            assert!(open(&path, &budget(&dir)).is_err());
        }
        // Prologs beyond the 32 MiB probe limit are refused, not opened as editable TIFFs.
        let huge = (MAX_XML_BYTES + 1024) as usize;
        for text in [
            format!("<!-- {} --><OME xmlns=\"{NS}\"/>", "x".repeat(huge)),
            format!("{}<OME xmlns=\"{NS}\"/>", " ".repeat(huge)),
        ] {
            write_tiff(&path, false, true, 1, Some(&text));
            assert!(detect(&path).unwrap());
            assert!(open(&path, &budget(&dir)).is_err());
        }
        write_tiff(&path, false, true, 1, Some("ImageJ=1.54\nimages=1\n"));
        assert!(!detect(&path).unwrap());
        // A member UUID after a long prolog is still compared with its reference.
        let reference = |uuid: &str| {
            ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 1, 1],
                    "",
                    &format!(r#"<TiffData><UUID FileName="b.ome.tif">{uuid}</UUID></TiffData>"#),
                ),
            )
        };
        write_tiff(
            &dir.join("b.ome.tif"),
            false,
            true,
            1,
            Some(&format!(
                r#"{comment}<OME xmlns="{NS}" UUID="urn:uuid:b"/>"#
            )),
        );
        assert!(opened(&dir, 1, &reference("urn:uuid:b")).is_ok());
        expect_error(
            opened(&dir, 1, &reference("urn:uuid:other")),
            "has UUID urn:uuid:b",
        );
        // An unreadable member root is an error, not an absent UUID.
        write_tiff(
            &dir.join("b.ome.tif"),
            false,
            true,
            1,
            Some(&format!("<!-- {}", "x".repeat(70 * 1024))),
        );
        expect_error(opened(&dir, 1, &reference("urn:uuid:b")), "cannot be read");
    }

    #[test]
    fn uuids_and_member_summaries_are_bounded_and_reserved() {
        let dir = temporary();
        let long = format!("urn:uuid:{}", "a".repeat(300));
        let reference = |file: &str, uuid: &str, z: u32| {
            format!(
                r#"<TiffData FirstZ="{z}" PlaneCount="1"><UUID FileName="{file}">{uuid}</UUID></TiffData>"#
            )
        };
        let xml = |body: String, z: u32| {
            ome(
                "urn:uuid:a",
                &image("A", "XYZCT", "uint8", [1, z, 1], "", &body),
            )
        };
        write_tiff(&dir.join("m0.tif"), false, true, 1, None);
        expect_error(
            opened(&dir, 1, &xml(reference("m0.tif", &long, 0), 1)),
            "UUID is longer",
        );
        expect_error(opened(&dir, 1, &ome(&long, "")), "UUID is longer");
        write_tiff(
            &dir.join("m0.tif"),
            false,
            true,
            1,
            Some(&format!(r#"<OME xmlns="{NS}" UUID="{long}"/>"#)),
        );
        expect_error(
            opened(&dir, 1, &xml(reference("m0.tif", "urn:uuid:m", 0), 1)),
            "UUID is longer",
        );
        // Many members: their summaries are admitted before resolution.
        let count = 300;
        for index in 0..count {
            write_tiff(&dir.join(format!("m{index}.tif")), false, true, 1, None);
        }
        let body: String = (0..count)
            .map(|index| {
                reference(
                    &format!("m{index}.tif"),
                    &format!("urn:uuid:{index}"),
                    index,
                )
            })
            .collect();
        let dataset = opened(&dir, 1, &xml(body.clone(), count)).unwrap();
        assert_eq!(dataset.members.len(), count as usize);
        let budget = budget(&dir);
        budget.set_limit(MIN_LIMIT_BYTES).unwrap();
        let available = budget.status().unwrap().available_bytes;
        let full = budget
            .try_reserve(available - u64::from(count) * MEMBER_BYTES / 2, false)
            .unwrap()
            .unwrap();
        let path = dir.join("many.ome.tif");
        write_tiff(&path, false, true, 1, Some(&xml(body, count)));
        let message = open(&path, &budget).map(|_| ()).unwrap_err();
        assert!(message.contains("memory budget"), "{message}");
        drop(full);
        assert_eq!(budget.status().unwrap().used_bytes, 0);
    }

    #[test]
    fn opened_companions_and_metadata_files_are_dependencies() {
        let dir = temporary();
        let data = dir.join("data.ome.tif");
        let companion = dir.join("set.companion.ome");
        write_tiff(
            &data,
            false,
            true,
            1,
            Some(&ome(
                "urn:uuid:data",
                r#"<BinaryOnly MetadataFile="set.companion.ome" UUID="urn:uuid:meta"/>"#,
            )),
        );
        std::fs::write(
            &companion,
            ome(
                "urn:uuid:meta",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 1, 1],
                    "",
                    r#"<TiffData><UUID FileName="data.ome.tif">urn:uuid:data</UUID></TiffData>"#,
                ),
            ),
        )
        .unwrap();
        for path in [&data, &companion] {
            let dataset = open(path, &budget(&dir)).unwrap().0;
            assert_eq!(dataset.members, [data.canonicalize().unwrap()]);
            assert_eq!(dataset.dependencies, [companion.canonicalize().unwrap()]);
        }
    }

    #[test]
    fn incompatible_series_are_skipped_without_resolving_them() {
        let dir = temporary();
        let mapped = r#"<TiffData IFD="0" PlaneCount="2"/>"#;
        let unresolved = r#"<TiffData><UUID FileName="../evil.tif">urn:uuid:e</UUID></TiffData>"#;
        let xml = ome(
            "urn:uuid:a",
            &[
                image("First", "XYZCT", "uint8", [1, 2, 1], "", mapped),
                image("Deeper", "XYZCT", "uint8", [1, 3, 1], "", unresolved),
                image("Float", "XYZCT", "float", [1, 2, 1], "", unresolved),
                image("Wide", "XYZCT", "uint16", [1, 2, 1], "", unresolved),
                image(
                    "Rgb",
                    "XYZCT",
                    "uint8",
                    [3, 2, 1],
                    r#"Interleaved="true""#,
                    &format!(r#"<Channel SamplesPerPixel="3"/>{unresolved}"#),
                ),
                image(
                    "",
                    "XYZTC",
                    "uint8",
                    [1, 2, 1],
                    "",
                    r#"<TiffData IFD="2" PlaneCount="2"/>"#,
                )
                .replace("SizeX=\"4\" SizeY=\"3\"", "SizeX=\"8\" SizeY=\"6\""),
            ]
            .concat(),
        );
        let dataset = opened(&dir, 4, &xml).unwrap();
        assert_eq!(dataset.skipped, 4);
        assert_eq!(
            dataset
                .series
                .iter()
                .map(|s| (s.name.as_str(), s.width))
                .collect::<Vec<_>>(),
            [("First", 4), ("Position 6", 8)]
        );
        assert_eq!(page(&dataset.series[1], 0, 1, 0), Some((0, 3)));
        expect_error(
            opened(
                &dir,
                4,
                &ome(
                    "urn:uuid:a",
                    &image("A", "XYZCT", "float", [1, 1, 1], "", mapped),
                ),
            ),
            "float pixels are not supported",
        );
    }

    #[test]
    fn logical_channels_are_scalar_or_one_rgb_channel() {
        let dir = temporary();
        let xml = |czt: [u32; 3], channels: &str| {
            ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYCZT",
                    "uint16",
                    czt,
                    "",
                    &format!("{channels}<TiffData/>"),
                ),
            )
        };
        // Without Channel elements, SizeC counts scalar channels.
        let series = &opened(&dir, 6, &xml([3, 2, 1], "")).unwrap().series[0];
        assert!(!series.rgb);
        assert_eq!(series.channels.len(), 3);
        assert_eq!(series.channels[2].color, "#0000ff");
        assert_eq!(page(series, 2, 1, 0), Some((0, 5)));
        // One RGB channel reads three bands from one page per Z/T.
        let rgb = r#"<Channel Name="Ignored" SamplesPerPixel="3"/>"#;
        let series = &opened(&dir, 4, &xml([3, 2, 2], rgb)).unwrap().series[0];
        assert!(series.rgb);
        assert_eq!(
            series
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.color.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Red", "#ff0000"),
                ("Green", "#00ff00"),
                ("Blue", "#0000ff")
            ]
        );
        assert_eq!(series.planes.len(), 4);
        for c in 0..3 {
            assert_eq!(page(series, c, 1, 1), Some((0, 3)));
        }
        assert_eq!(page(series, 3, 0, 0), None);
        for (czt, channels, message) in [
            (
                [4, 1, 1],
                r#"<Channel SamplesPerPixel="3"/><Channel/>"#,
                "SamplesPerPixel [3, 1]",
            ),
            (
                [6, 1, 1],
                r#"<Channel SamplesPerPixel="3"/><Channel SamplesPerPixel="3"/>"#,
                "SamplesPerPixel [3, 3]",
            ),
            (
                [2, 1, 1],
                r#"<Channel SamplesPerPixel="2"/>"#,
                "SamplesPerPixel [2]",
            ),
            (
                [4, 1, 1],
                r#"<Channel SamplesPerPixel="4"/>"#,
                "SamplesPerPixel [4]",
            ),
            (
                [3, 1, 1],
                r#"<Channel/><Channel/>"#,
                "do not add up to SizeC=3",
            ),
            ([33, 1, 1], "", "at most 32 channels"),
        ] {
            expect_error(opened(&dir, 6, &xml(czt, channels)), message);
        }
        let many = "<Channel/>".repeat(33);
        expect_error(
            opened(&dir, 6, &xml([33, 1, 1], &many)),
            "at most 32 channels",
        );
    }

    #[test]
    fn colors_units_and_sample_types() {
        assert_eq!(color("-16776961").as_deref(), Some("#ff0000"));
        assert_eq!(color("-1").as_deref(), Some("#ffffff"));
        assert_eq!(color("16711935").as_deref(), Some("#00ff00"));
        assert_eq!(color("65535").as_deref(), Some("#0000ff"));
        assert_eq!(color("4294967295"), None);
        let dir = temporary();
        let calibrated = |extra: &str| {
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYZCT",
                    "uint8",
                    [1, 1, 1],
                    extra,
                    r#"<Channel Color="-16776961"/><TiffData/>"#,
                ),
            );
            opened(&dir, 1, &xml).unwrap().series.remove(0)
        };
        let close = |value: Option<f64>, expected: f64| {
            assert!(
                (value.unwrap() - expected).abs() < 1e-12 * expected.max(1.0),
                "{value:?} != {expected}"
            )
        };
        let series = calibrated(
            r#"PhysicalSizeX="0.5" PhysicalSizeY="2" PhysicalSizeZ="3" TimeIncrement="1.5" SignificantBits="12""#,
        );
        assert_eq!(series.channels[0].color, "#ff0000");
        assert_eq!(series.channels[0].domain, [0, 255]);
        assert_eq!(series.sample_bits, 8);
        close(series.calibration.pixel_width_um, 0.5);
        close(series.calibration.pixel_height_um, 2.0);
        close(series.calibration.z_step_um, 3.0);
        close(series.calibration.time_increment_seconds, 1.5);
        for (unit, expected) in [
            ("nm", 0.5e-3),
            ("\u{b5}m", 0.5),
            ("\u{3bc}m", 0.5),
            ("um", 0.5),
            ("mm", 500.0),
            ("cm", 5000.0),
            ("m", 0.5e6),
            ("\u{c5}", 0.5e-4),
            ("\u{212b}", 0.5e-4),
        ] {
            close(
                calibrated(&format!(
                    r#"PhysicalSizeX="0.5" PhysicalSizeXUnit="{unit}""#
                ))
                .calibration
                .pixel_width_um,
                expected,
            );
        }
        let series = calibrated(
            r#"PhysicalSizeX="0.5" PhysicalSizeXUnit="pixel" PhysicalSizeY="-1" PhysicalSizeZ="nan""#,
        );
        assert_eq!(
            (
                series.calibration.pixel_width_um,
                series.calibration.pixel_height_um,
                series.calibration.z_step_um
            ),
            (None, None, None)
        );
        for (unit, expected) in [("ms", 1.5e-3), ("s", 1.5), ("min", 90.0), ("h", 5400.0)] {
            close(
                calibrated(&format!(
                    r#"TimeIncrement="1.5" TimeIncrementUnit="{unit}""#
                ))
                .calibration
                .time_increment_seconds,
                expected,
            );
        }
        assert_eq!(
            calibrated(r#"TimeIncrement="1" TimeIncrementUnit="fortnight""#)
                .calibration
                .time_increment_seconds,
            None
        );
        let wide = |bits: &str| {
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "A",
                    "XYZCT",
                    "uint16",
                    [1, 1, 1],
                    &format!(r#"SignificantBits="{bits}""#),
                    "<TiffData/>",
                ),
            );
            opened(&dir, 1, &xml).unwrap().series.remove(0)
        };
        // Display domains follow the sample width, as for other 16-bit images.
        let series = wide("12");
        assert_eq!(series.sample_bits, 16);
        assert_eq!(series.channels[0].domain, [0, 65535]);
        assert_eq!(series.channels[0].color, "#ffffff");
        for kind in ["int16", "float", "double", "bit"] {
            let xml = ome(
                "urn:uuid:a",
                &image("A", "XYZCT", kind, [1, 1, 1], "", "<TiffData/>"),
            );
            expect_error(
                opened(&dir, 1, &xml),
                &format!("{kind} pixels are not supported"),
            );
        }
    }
}
