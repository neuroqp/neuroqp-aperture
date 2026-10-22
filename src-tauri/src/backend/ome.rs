//! OME-TIFF browsing. A selection maps to TIFF pages in member files that
//! libvips reads directly through job-local aliases, so no frame is
//! materialized and the 256 MiB frame limit applies only to projections. Max
//! projections reduce one page at a time into an accumulator and reuse the
//! display-TIFF path of ND2/CZI.

use super::{
    process::run,
    pyramid::{at_page, validate_header},
    Job,
};
use crate::{
    channels::{Layout, Plane},
    frame_cache::FRAME_OVERHEAD,
    frames::{
        self, check_cancel, Dimensions, ExtractedFrame, RawFrame, Selection, CANCEL_CHUNK_SAMPLES,
        MAX_FRAME_BYTES,
    },
    instances::{fingerprint, Fingerprint},
    memory::{Budget, Reservation},
    native::{self, NativeLevel},
    ome_tiff::{self, PlaneRef, Series},
};
use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, HashMap},
    ffi::OsString,
    fs::File,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

/// An opened dataset and the identity of every file it depends on (the
/// plan's `MemberSet`): members, then an opened companion or `BinaryOnly`
/// metadata file. Paths are fingerprinted rather than held open: datasets can
/// have thousands of members, above common open-file limits.
pub(crate) struct Source {
    dataset: ome_tiff::Dataset,
    fingerprints: Vec<Fingerprint>,
    /// Hash of every file's path and identity, part of display-cache keys.
    pub identity: u64,
    _metadata: Reservation,
    _fingerprints: Reservation,
}

/// One selected frame's navigation metadata and geometry.
pub(crate) struct Frame {
    pub metadata: ExtractedFrame,
    pub width: u32,
    pub height: u32,
    pub projection_unavailable: Option<String>,
    pub skipped_series: u32,
}

impl Source {
    pub fn open(path: &Path, budget: &Arc<Budget>) -> Result<Self, String> {
        let (dataset, metadata) = ome_tiff::open(path, budget)?;
        let files = dataset.members.len() + dataset.dependencies.len();
        let reservation = budget.require(
            (files * std::mem::size_of::<Fingerprint>()) as u64,
            "OME-TIFF: metadata does not fit the application memory budget.",
        )?;
        let mut hasher = DefaultHasher::new();
        let fingerprints = Self::files(&dataset)
            .map(|member| {
                let print = fingerprint(member)
                    .ok()
                    .flatten()
                    .ok_or_else(|| changed(member))?;
                (member, &print).hash(&mut hasher);
                Ok(print)
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            dataset,
            fingerprints,
            identity: hasher.finish(),
            _metadata: metadata,
            _fingerprints: reservation,
        })
    }

    fn files(dataset: &ome_tiff::Dataset) -> impl Iterator<Item = &PathBuf> {
        dataset.members.iter().chain(&dataset.dependencies)
    }

    /// A changed or missing file fails the selection; the user reopens.
    pub fn verify(&self) -> Result<(), String> {
        for (file, expected) in Self::files(&self.dataset).zip(&self.fingerprints) {
            check(file, expected)?;
        }
        Ok(())
    }

    /// Checks one member before and after reading pixels from it on demand.
    pub fn verify_member(&self, member: u32) -> Result<(), String> {
        let expected = self
            .fingerprints
            .get(member as usize)
            .filter(|_| (member as usize) < self.dataset.members.len())
            .ok_or("Invalid OME-TIFF member file.")?;
        check(self.member(member)?, expected)
    }

    pub fn member(&self, index: u32) -> Result<&Path, String> {
        self.dataset
            .members
            .get(index as usize)
            .map(PathBuf::as_path)
            .ok_or_else(|| "Invalid OME-TIFF member file.".into())
    }

    fn series(&self, position: u32) -> Result<&Series, String> {
        self.dataset
            .series
            .get(position as usize)
            .ok_or_else(|| "OME-TIFF position is out of range.".into())
    }

    pub fn describe(&self, selection: Selection) -> Result<Frame, String> {
        let series = self.series(selection.position)?;
        if selection.z >= series.size_z || selection.time >= series.size_t {
            return Err("OME-TIFF Z slice or timepoint is out of range.".into());
        }
        let calibration = &series.calibration;
        let calibrated = calibration.pixel_width_um.is_some()
            || calibration.pixel_height_um.is_some()
            || calibration.z_step_um.is_some()
            || calibration.time_increment_seconds.is_some();
        let metadata = ExtractedFrame {
            dimensions: Dimensions {
                positions: self.dataset.series.iter().map(|s| s.name.clone()).collect(),
                z: series.size_z,
                time: series.size_t,
                position: selection.position,
                z_index: selection.z,
                time_index: selection.time,
            },
            channels: series.channels.clone(),
            calibration: calibrated.then(|| calibration.clone()),
            display_ranges: Vec::new(),
            sample_bits: series.sample_bits,
            rgb: series.rgb,
        };
        // Navigation metadata is part of every state update and cache manifest.
        if serde_json::to_vec(&metadata)
            .map_err(|e| e.to_string())?
            .len()
            > 48 * 1024
        {
            return Err("OME-TIFF position/channel names exceed the 48 KiB metadata limit.".into());
        }
        Ok(Frame {
            metadata,
            width: series.width,
            height: series.height,
            projection_unavailable: projection_reason(series),
            skipped_series: self.dataset.skipped as u32,
        })
    }

    /// Display planes of a selection; an unmapped plane is a recoverable error.
    pub fn planes(&self, selection: Selection) -> Result<Vec<Plane>, String> {
        let series = self.series(selection.position)?;
        let mut planes = Vec::new();
        for c in 0..logical_channels(series) {
            let page = stored(series, c, selection.z, selection.time, "")?;
            let bands = if series.rgb { 0..3 } else { 0..1 };
            planes.extend(bands.map(|band| Plane {
                member: page.member.into(),
                page: page.ifd,
                band,
            }));
        }
        Ok(planes)
    }
}

fn check(file: &Path, expected: &Fingerprint) -> Result<(), String> {
    if fingerprint(file).ok().flatten().as_ref() == Some(expected) {
        Ok(())
    } else {
        Err(changed(file))
    }
}

fn changed(member: &Path) -> String {
    format!(
        "OME-TIFF dataset changed on disk; reopen it. Changed or missing file: {}",
        member
            .file_name()
            .unwrap_or(member.as_os_str())
            .to_string_lossy()
    )
}

fn logical_channels(series: &Series) -> u32 {
    if series.rgb {
        1
    } else {
        series.channels.len() as u32
    }
}

/// The page of logical channel `c`, numbered from 1 in messages as in the UI.
fn stored(series: &Series, c: u32, z: u32, t: u32, scope: &str) -> Result<PlaneRef, String> {
    series.plane(c, z, t).ok_or_else(|| {
        format!(
            "OME-TIFF plane C {}, Z {}, T {}{scope} is not stored in this dataset.",
            c + 1,
            z + 1,
            t + 1
        )
    })
}

fn frame_bytes(series: &Series) -> u128 {
    u128::from(series.width)
        * u128::from(series.height)
        * series.channels.len() as u128
        * u128::from(series.sample_bits / 8)
}

/// Projections accumulate a whole frame, unlike ordinary plane browsing.
fn projection_reason(series: &Series) -> Option<String> {
    let bytes = frame_bytes(series);
    (bytes > u128::from(MAX_FRAME_BYTES)).then(|| {
        format!(
            "Max projections are unavailable: this {} × {} frame with {} channel(s) needs {:.1} MiB, above the 256 MiB projection limit.",
            series.width,
            series.height,
            series.channels.len(),
            bytes as f64 / (1024.0 * 1024.0)
        )
    })
}

/// Job-local, loader-safe alias of a member file.
fn alias(job: &Job, source: &Source, member: u32) -> Result<(), String> {
    let path = job.member_alias(member);
    if !path.exists() {
        crate::platform::source_alias(source.member(member)?, &path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Opens the dataset and seeds the selection's state before any cache reuse:
/// members are verified, the cache key gains their identity and selected
/// planes get member aliases.
pub(super) fn prepare(job: &Job) -> Result<Arc<Source>, String> {
    let acquisition = job.acquisition.as_ref().ok_or("Missing acquisition")?;
    let selection = job.selection.ok_or("Missing selection")?;
    let source = acquisition.ome()?;
    source.verify()?;
    let frame = source.describe(selection)?;
    job.set_member_identity(source.identity);
    let _ = job.ome.set(source.clone());
    job.update(|state| {
        state.width = frame.width;
        state.height = frame.height;
        state.dimensions = Some(frame.metadata.dimensions);
        state.channels = frame.metadata.channels;
        state.channel_mode = true;
        state.sample_bits = frame.metadata.sample_bits;
        state.rgb = frame.metadata.rgb;
        state.calibration = frame.metadata.calibration;
        state.skipped_series = frame.skipped_series;
        state.projection_unavailable = frame.projection_unavailable.clone();
    });
    if job.projection == frames::Projection::None {
        let planes = source.planes(selection)?;
        for plane in &planes {
            alias(job, &source, plane.member)?;
        }
        *job.planes.lock().unwrap() = planes;
    } else if let Some(reason) = frame.projection_unavailable {
        return Err(reason);
    }
    Ok(source)
}

/// Validates each selected page against its `Pixels` and discovers native
/// levels per page. Levels are stored relative to the page (`page=0` means
/// the plane's own page) and reused only when every plane has the same ones.
pub(super) fn layout(
    job: &Job,
    source: &Source,
) -> Result<(String, Layout, Vec<NativeLevel>), String> {
    let selection = job.selection.ok_or("Missing selection")?;
    let series = source.series(selection.position)?;
    let planes = job.planes.lock().unwrap().clone();
    let mut first = None;
    let mut levels: Option<Vec<NativeLevel>> = None;
    for (index, plane) in planes.iter().enumerate() {
        if planes[..index]
            .iter()
            .any(|seen| (seen.member, seen.page) == (plane.member, plane.page))
        {
            continue;
        }
        let headers = RefCell::new(HashMap::<String, Result<String, String>>::new());
        let inspect = |options: &str| {
            headers
                .borrow_mut()
                .entry(options.to_string())
                .or_insert_with(|| {
                    run(
                        job,
                        "vipsheader",
                        &["-a".into(), plane_input(job, plane, options)],
                    )
                })
                .clone()
        };
        let header = inspect("[page=0,n=1]")?;
        let pixels = validate_header(&header)?;
        let bands = if series.rgb { 3 } else { 1 };
        if !pixels.tiff
            || (pixels.width, pixels.height) != (series.width, series.height)
            || pixels.bands != bands
            || pixels.ushort != (series.sample_bits == 16)
        {
            return Err(format!(
                "OME-TIFF page {} of {} does not match its Pixels metadata: expected {} × {} pixels with {bands} sample(s) of {} bits.",
                plane.page,
                source
                    .member(plane.member)?
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                series.width,
                series.height,
                series.sample_bits
            ));
        }
        let found = native::discover(&header, &inspect, None);
        levels = Some(match levels {
            None => found,
            Some(previous) if same_levels(&previous, &found) => previous,
            Some(_) => Vec::new(),
        });
        first.get_or_insert(header);
    }
    let header = first.ok_or("OME-TIFF selection has no planes.")?;
    Ok((
        header,
        Layout {
            channels: series.channels.clone(),
            planes,
            channel_mode: true,
            rgb: series.rgb,
        },
        levels.unwrap_or_default(),
    ))
}

fn same_levels(a: &[NativeLevel], b: &[NativeLevel]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| (a.width, a.height, &a.options) == (b.width, b.height, &b.options))
}

/// libvips input for `plane`: its member file with `options`, where page 0
/// names the plane's own page.
pub(super) fn plane_input(job: &Job, plane: &Plane, options: &str) -> OsString {
    let mut input = job.plane_path(plane).into_os_string();
    input.push(at_page(options, plane.page));
    input
}

/// Maximum over Z and/or time, one page at a time, written as the job's
/// display TIFF. Every page passes through a `.v` scratch file, so the same
/// reducer works with the image worker and the libvips tools, and the
/// accumulator plus one 64 Ki-sample chunk is the only retained pixel data.
pub(super) fn project(
    job: &Job,
    source: &Source,
    progress: impl Fn(f64),
) -> Result<ExtractedFrame, String> {
    let selection = job.selection.ok_or("Missing selection")?;
    let projection = job.projection;
    let series = source.series(selection.position)?;
    let frame = source.describe(selection)?;
    if let Some(reason) = frame.projection_unavailable {
        return Err(reason);
    }
    let ranges = [
        (0, logical_channels(series)),
        if projection.projects_z() {
            (0, series.size_z)
        } else {
            (selection.z, selection.z + 1)
        },
        if projection.projects_time() {
            (0, series.size_t)
        } else {
            (selection.time, selection.time + 1)
        },
    ];
    // Fail on an unmapped plane before admitting or reading anything.
    let mut total = 0;
    each_page(ranges, |c, z, t| {
        total += 1;
        alias(
            job,
            source,
            stored(series, c, z, t, " in the projected range")?
                .member
                .into(),
        )
    })?;
    let bytes = usize::from(series.sample_bits / 8);
    let bands = series.channels.len();
    let length = frame_bytes(series) as u64;
    let page = length / u64::from(logical_channels(series));
    let chunk = (CANCEL_CHUNK_SAMPLES * bands * bytes) as u64;
    // The accumulator, two copies of a page and the read chunk, admitted before
    // any worker call and outside cache locks; tools reserve their own workspace.
    let _reservation = job
        .memory
        .reserve(length + 2 * page + chunk + FRAME_OVERHEAD, false, Some(&job.cancel))?
        .ok_or("OME-TIFF projection does not fit the application memory budget. Increase the limit in Settings.")?;
    check_cancel(Some(&job.cancel))?;
    let mut accumulator = vec![0u8; length as usize];
    let scratch = job.work.join("projection-page.v");
    let mut done = 0;
    each_page(ranges, |c, z, t| {
        check_cancel(Some(&job.cancel))?;
        let page = stored(series, c, z, t, " in the projected range")?;
        let plane = Plane {
            member: page.member.into(),
            page: page.ifd,
            band: 0,
        };
        // Projected pages are read through member aliases, not the output TIFF.
        let mut input = job.member_alias(plane.member).into_os_string();
        input.push(format!("[page={},n=1,access=sequential]", plane.page));
        let reduced = run(
            job,
            "vips",
            &["copy".into(), input, scratch.clone().into_os_string()],
        )
        .and_then(|_| {
            reduce_page(
                &scratch,
                &mut accumulator,
                (series.width, series.height, bands, bytes),
                (!series.rgb).then_some(c as usize),
                &job.cancel,
            )
        });
        let _ = std::fs::remove_file(&scratch);
        reduced?;
        done += 1;
        progress(0.6 * done as f64 / total as f64);
        Ok(())
    })?;
    source.verify()?;
    let raw = RawFrame {
        width: series.width,
        height: series.height,
        bands: bands as u32,
        bytes,
        rgb: series.rgb,
        pixels: accumulator,
    };
    frames::write_frame(&raw, frame.metadata, &job.input, Some(&job.cancel))
}

/// Visits (C, Z, T) in time, Z, channel order over half-open `ranges`.
fn each_page(
    ranges: [(u32, u32); 3],
    mut visit: impl FnMut(u32, u32, u32) -> Result<(), String>,
) -> Result<(), String> {
    for t in ranges[2].0..ranges[2].1 {
        for z in ranges[1].0..ranges[1].1 {
            for c in ranges[0].0..ranges[0].1 {
                visit(c, z, t)?;
            }
        }
    }
    Ok(())
}

/// Maximum of one `.v` page into an interleaved accumulator: scalar pages fill
/// `channel`, RGB pages all three bands. Cancellation is checked every chunk.
fn reduce_page(
    page: &Path,
    accumulator: &mut [u8],
    (width, height, bands, bytes): (u32, u32, usize, usize),
    channel: Option<usize>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let error = |e: std::io::Error| format!("OME-TIFF projection: {e}");
    let mut file = File::open(page).map_err(error)?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header).map_err(error)?;
    let little = match header[..4] {
        [0xb6, 0xa6, 0xf2, 0x08] => true,
        [0x08, 0xf2, 0xa6, 0xb6] => false,
        _ => return Err("OME-TIFF projection page is not a libvips image.".into()),
    };
    let field = |at: usize| {
        let raw = header[at..at + 4].try_into().unwrap();
        if little {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        }
    };
    let page_bands = if channel.is_some() { 1 } else { bands };
    // BandFmt: 0 uchar, 2 ushort; coding 0 is uncoded pixels.
    if (field(4), field(8), field(12) as usize, field(20), field(24))
        != (width, height, page_bands, if bytes == 2 { 2 } else { 0 }, 0)
    {
        return Err("OME-TIFF projection page differs from its Pixels metadata.".into());
    }
    let pixels = width as usize * height as usize;
    let stride = bands * bytes;
    let mut chunk = vec![0u8; CANCEL_CHUNK_SAMPLES * page_bands * bytes];
    let mut pixel = 0;
    while pixel < pixels {
        check_cancel(Some(cancel))?;
        let count = (pixels - pixel).min(CANCEL_CHUNK_SAMPLES);
        let data = &mut chunk[..count * page_bands * bytes];
        file.read_exact(data).map_err(error)?;
        let output = &mut accumulator[pixel * stride..(pixel + count) * stride];
        for (input, output) in data
            .chunks_exact(page_bands * bytes)
            .zip(output.chunks_exact_mut(stride))
        {
            let output = match channel {
                Some(c) => &mut output[c * bytes..(c + 1) * bytes],
                None => output,
            };
            for (input, output) in input
                .chunks_exact(bytes)
                .zip(output.chunks_exact_mut(bytes))
            {
                if bytes == 1 {
                    output[0] = output[0].max(input[0]);
                } else {
                    let value = if little {
                        u16::from_le_bytes([input[0], input[1]])
                    } else {
                        u16::from_be_bytes([input[0], input[1]])
                    };
                    let maximum = u16::from_le_bytes([output[0], output[1]]).max(value);
                    output.copy_from_slice(&maximum.to_le_bytes());
                }
            }
        }
        pixel += count;
    }
    check_cancel(Some(cancel))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::backend::{
        assets::{max_level, read_asset, u16_body, Asset},
        tests::wait_ready,
        Backend, ImageState, Phase, NEXT_ID,
    };
    use crate::frames::Projection;
    use std::{
        fs,
        process::Command,
        sync::atomic::Ordering,
        thread,
        time::{Duration, Instant},
    };

    const NS: &str = "http://www.openmicroscopy.org/Schemas/OME/2016-06";

    /// A TIFF page of interleaved samples, optionally tiled, with SubIFD levels.
    pub(crate) struct Page {
        pub width: u32,
        pub height: u32,
        pub samples: u16,
        pub bits: u16,
        pub data: Vec<u16>,
        pub tile: Option<u32>,
        pub subifds: Vec<Page>,
    }

    pub(crate) fn page(
        (width, height): (u32, u32),
        samples: u16,
        bits: u16,
        value: impl Fn(u32, u32, u16) -> u16,
    ) -> Page {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                for band in 0..samples {
                    data.push(value(x, y, band));
                }
            }
        }
        Page {
            width,
            height,
            samples,
            bits,
            data,
            tile: None,
            subifds: Vec::new(),
        }
    }

    fn put(out: &mut Vec<u8>, bytes: &[u8]) -> u32 {
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let at = out.len() as u32;
        out.extend_from_slice(bytes);
        at
    }

    fn write_ifd(out: &mut Vec<u8>, page: &Page, description: Option<&str>, reduced: bool) -> u32 {
        let children: Vec<u32> = page
            .subifds
            .iter()
            .map(|child| write_ifd(out, child, None, true))
            .collect();
        let encode = |values: &[u16]| -> Vec<u8> {
            if page.bits == 8 {
                values.iter().map(|v| *v as u8).collect()
            } else {
                values.iter().flat_map(|v| v.to_le_bytes()).collect()
            }
        };
        let samples = usize::from(page.samples);
        let mut entries: Vec<(u16, u16, Vec<u8>)> = Vec::new();
        let short = |values: &[u32]| {
            values
                .iter()
                .flat_map(|v| (*v as u16).to_le_bytes())
                .collect()
        };
        let long = |values: &[u32]| values.iter().flat_map(|v| v.to_le_bytes()).collect();
        match page.tile {
            None => {
                let bytes = encode(&page.data);
                let at = put(out, &bytes);
                entries.push((273, 4, long(&[at])));
                entries.push((278, 4, long(&[page.height])));
                entries.push((279, 4, long(&[bytes.len() as u32])));
            }
            Some(tile) => {
                let (mut offsets, mut counts) = (Vec::new(), Vec::new());
                for ty in 0..page.height.div_ceil(tile) {
                    for tx in 0..page.width.div_ceil(tile) {
                        let mut values = vec![0u16; (tile * tile) as usize * samples];
                        for row in 0..tile {
                            for column in 0..tile {
                                let (x, y) = (tx * tile + column, ty * tile + row);
                                if x < page.width && y < page.height {
                                    let from = (y * page.width + x) as usize * samples;
                                    let to = (row * tile + column) as usize * samples;
                                    values[to..to + samples]
                                        .copy_from_slice(&page.data[from..from + samples]);
                                }
                            }
                        }
                        let bytes = encode(&values);
                        offsets.push(put(out, &bytes));
                        counts.push(bytes.len() as u32);
                    }
                }
                entries.push((322, 4, long(&[tile])));
                entries.push((323, 4, long(&[tile])));
                entries.push((324, 4, long(&offsets)));
                entries.push((325, 4, long(&counts)));
            }
        }
        if reduced {
            entries.push((254, 4, long(&[1])));
        }
        entries.push((256, 4, long(&[page.width])));
        entries.push((257, 4, long(&[page.height])));
        entries.push((258, 3, short(&vec![u32::from(page.bits); samples])));
        entries.push((259, 3, short(&[1])));
        entries.push((262, 3, short(&[if samples == 3 { 2 } else { 1 }])));
        if let Some(text) = description {
            entries.push((270, 2, [text.as_bytes(), b"\0"].concat()));
        }
        entries.push((277, 3, short(&[samples as u32])));
        entries.push((284, 3, short(&[1])));
        if !children.is_empty() {
            entries.push((330, 4, long(&children)));
        }
        entries.push((339, 3, short(&vec![1; samples])));
        entries.sort_by_key(|entry| entry.0);
        let entries: Vec<_> = entries
            .into_iter()
            .map(|(tag, kind, bytes)| {
                let size = match kind {
                    2 => 1,
                    3 => 2,
                    _ => 4,
                };
                let count = (bytes.len() / size) as u32;
                let value = if bytes.len() <= 4 {
                    let mut inline = [0u8; 4];
                    inline[..bytes.len()].copy_from_slice(&bytes);
                    inline
                } else {
                    put(out, &bytes).to_le_bytes()
                };
                (tag, kind, count, value)
            })
            .collect();
        let at = put(out, &(entries.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value);
        }
        out.extend(0u32.to_le_bytes());
        at
    }

    /// Classic little-endian TIFF; the first page carries `description`.
    pub(crate) fn write_tiff(path: &Path, pages: &[Page], description: Option<&str>) {
        let mut out = b"II*\0\0\0\0\0".to_vec();
        let mut link = 4;
        for (index, page) in pages.iter().enumerate() {
            let at = write_ifd(&mut out, page, description.filter(|_| index == 0), false);
            out[link..link + 4].copy_from_slice(&at.to_le_bytes());
            let count = u16::from_le_bytes([out[at as usize], out[at as usize + 1]]) as usize;
            link = at as usize + 2 + count * 12;
        }
        fs::write(path, out).unwrap();
    }

    pub(crate) fn ome(uuid: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><OME xmlns="{NS}" UUID="{uuid}">{body}</OME>"#
        )
    }

    /// One XYCZT image; `channels` and `data` are the Pixels children.
    pub(crate) fn image(
        name: &str,
        kind: &str,
        (width, height): (u32, u32),
        [c, z, t]: [u32; 3],
        channels: &str,
        data: &str,
    ) -> String {
        format!(
            r#"<Image ID="Image:{name}" Name="{name}"><Pixels ID="Pixels:{name}" DimensionOrder="XYCZT" Type="{kind}" SizeX="{width}" SizeY="{height}" SizeC="{c}" SizeZ="{z}" SizeT="{t}" PhysicalSizeX="0.25" PhysicalSizeY="0.25" PhysicalSizeZ="1.5" TimeIncrement="2">{channels}{data}</Pixels></Image>"#
        )
    }

    /// Distinct samples per member, page, position and band.
    pub(crate) fn value(bits: u16, member: u32, ifd: u32, x: u32, y: u32, band: u16) -> u16 {
        let raw = 1000 + member * 7919 + ifd * 1301 + y * 31 + x * 3 + u32::from(band) * 5003;
        if bits == 8 {
            (raw % 251) as u16
        } else {
            (raw % 60000) as u16
        }
    }

    fn plane_pages(
        bits: u16,
        member: u32,
        count: u32,
        size: (u32, u32),
        samples: u16,
    ) -> Vec<Page> {
        (0..count)
            .map(|ifd| {
                page(size, samples, bits, |x, y, band| {
                    value(bits, member, ifd, x, y, band)
                })
            })
            .collect()
    }

    pub(crate) fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aperture-ome-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn vips() -> bool {
        Command::new("vips").arg("--version").output().is_ok()
    }

    fn wait_phase(backend: &Backend) -> ImageState {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = backend.snapshot().unwrap();
            if state.phase != Phase::Loading {
                return state;
            }
            assert!(Instant::now() < deadline, "{state:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Samples of one asset: `.u16` bodies for 16-bit, 8-bit gray PNGs otherwise.
    pub(crate) fn samples(backend: &Backend, asset: Asset) -> Vec<u16> {
        let job = backend.current.lock().unwrap().clone().unwrap();
        let png = read_asset(&job, asset).unwrap();
        if job.state.lock().unwrap().sample_bits == 16 {
            let body = u16_body(&png, |_, _| true).unwrap();
            body[8..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| u16::from_le_bytes(*s))
                .collect()
        } else {
            let mut reader = png::Decoder::new(std::io::Cursor::new(png))
                .read_info()
                .unwrap();
            let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
            reader.next_frame(&mut buffer).unwrap();
            buffer.into_iter().map(u16::from).collect()
        }
    }

    fn expected(
        bits: u16,
        member: u32,
        ifd: u32,
        (width, height): (u32, u32),
        band: u16,
    ) -> Vec<u16> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| value(bits, member, ifd, x, y, band)))
            .collect()
    }

    fn select(backend: &Backend, z: u32, time: u32, projection: Option<Projection>) -> ImageState {
        let id = backend.snapshot().unwrap().id;
        backend
            .select_view(
                &id,
                Selection {
                    position: 0,
                    z,
                    time,
                },
                projection,
            )
            .unwrap();
        wait_phase(backend)
    }

    fn channel_assets(size: (u32, u32), channel: u32) -> [Asset; 2] {
        [
            Asset::ChannelPreview { channel },
            Asset::ChannelTile {
                channel,
                level: max_level(size.0, size.1),
                x: 0,
                y: 0,
            },
        ]
    }

    #[test]
    fn single_file_planes_show_exact_samples_for_every_c_z_t() {
        if !vips() {
            return;
        }
        let root = root("single");
        let backend = Backend::new(root.join("cache")).unwrap();
        let size = (20, 12);
        for bits in [8, 16] {
            let kind = if bits == 8 { "uint8" } else { "uint16" };
            // Detection is by content: plain .tif and .ome.tiff names both work.
            let extension = if bits == 8 { "tif" } else { "ome.tiff" };
            let source = root.join(format!("cells-{bits} [z].{extension}"));
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "Well A1",
                    kind,
                    size,
                    [2, 3, 2],
                    r#"<Channel Name="DAPI" Color="65535"/><Channel Name="GFP" Color="16711935"/>"#,
                    r#"<TiffData IFD="0" PlaneCount="12"/>"#,
                ),
            );
            write_tiff(&source, &plane_pages(bits, 0, 12, size, 1), Some(&xml));
            let original = fs::read(&source).unwrap();
            backend.open(&source).unwrap();
            let state = wait_ready(&backend);
            assert!(
                state.read_only && state.channel_mode && !state.rgb,
                "{state:?}"
            );
            assert_eq!(state.sample_bits as u16, bits);
            assert_eq!((state.width, state.height), size);
            assert_eq!(
                state
                    .channels
                    .iter()
                    .map(|c| (c.name.as_str(), c.color.as_str()))
                    .collect::<Vec<_>>(),
                [("DAPI", "#0000ff"), ("GFP", "#00ff00")]
            );
            let dimensions = state.dimensions.clone().unwrap();
            assert_eq!(dimensions.positions, ["Well A1"]);
            assert_eq!((dimensions.z, dimensions.time), (3, 2));
            assert_eq!(state.calibration.unwrap().z_step_um, Some(1.5));
            assert_eq!(state.skipped_series, 0);
            assert_eq!(state.projection_unavailable, None);
            for (z, time) in [(0, 0), (2, 1), (1, 0)] {
                if (z, time) != (0, 0) {
                    assert_eq!(select(&backend, z, time, None).phase, Phase::Ready);
                }
                for c in 0..2 {
                    let ifd = c + 2 * (z + 3 * time);
                    for asset in channel_assets(size, c) {
                        assert_eq!(
                            samples(&backend, asset),
                            expected(bits, 0, ifd, size, 0),
                            "{bits} c{c} z{z} t{time}"
                        );
                    }
                }
            }
            // A repeated selection reuses its display cache.
            assert!(select(&backend, 2, 1, None).detail.contains("Reused"));
            assert_eq!(fs::read(&source).unwrap(), original);
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multi_file_sets_binary_only_and_companions_resolve_every_member() {
        if !vips() {
            return;
        }
        let root = root("multi");
        let size = (9, 7);
        let data = r#"<TiffData FirstZ="0" PlaneCount="2"><UUID FileName="a.ome.tif">urn:uuid:a</UUID></TiffData><TiffData FirstZ="2" PlaneCount="2"><UUID FileName="b.ome.tif">urn:uuid:b</UUID></TiffData>"#;
        let full = |uuid: &str| ome(uuid, &image("Set", "uint16", size, [1, 4, 1], "", data));
        let a = root.join("a.ome.tif");
        let b = root.join("b.ome.tif");
        write_tiff(
            &a,
            &plane_pages(16, 0, 2, size, 1),
            Some(&full("urn:uuid:a")),
        );
        write_tiff(
            &b,
            &plane_pages(16, 1, 2, size, 1),
            Some(&full("urn:uuid:b")),
        );
        let backend = Backend::new(root.join("cache")).unwrap();
        let check = |backend: &Backend| {
            for z in 0..4 {
                let state = select(backend, z, 0, None);
                assert_eq!(state.phase, Phase::Ready, "{state:?}");
                let (member, ifd) = (z / 2, z % 2);
                assert_eq!(
                    samples(backend, Asset::ChannelPreview { channel: 0 }),
                    expected(16, member, ifd, size, 0)
                );
            }
        };
        backend.open(&b).unwrap();
        wait_ready(&backend);
        check(&backend);
        // BinaryOnly members and a companion opened directly.
        let companion = root.join("set.companion.ome");
        fs::write(&companion, full("urn:uuid:meta")).unwrap();
        let binary = |uuid: &str| {
            ome(
                uuid,
                r#"<BinaryOnly MetadataFile="set.companion.ome" UUID="urn:uuid:meta"/>"#,
            )
        };
        write_tiff(
            &a,
            &plane_pages(16, 0, 2, size, 1),
            Some(&binary("urn:uuid:a")),
        );
        write_tiff(
            &b,
            &plane_pages(16, 1, 2, size, 1),
            Some(&binary("urn:uuid:b")),
        );
        for opened in [&companion, &a] {
            backend.open(opened).unwrap();
            let state = wait_ready(&backend);
            assert!(state.read_only);
            check(&backend);
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rgb16_series_browse_z_and_time_and_skip_other_series() {
        if !vips() {
            return;
        }
        let root = root("rgb");
        let size = (11, 6);
        let other = image(
            "Label",
            "uint8",
            (4, 4),
            [1, 1, 1],
            "",
            r#"<TiffData IFD="4"/>"#,
        );
        let xml = ome(
            "urn:uuid:a",
            &format!(
                "{}{other}",
                image(
                    "RGB",
                    "uint16",
                    size,
                    [3, 2, 2],
                    r#"<Channel SamplesPerPixel="3"/>"#,
                    r#"<TiffData PlaneCount="4"/>"#
                )
            ),
        );
        let source = root.join("rgb.ome.tif");
        let mut pages = plane_pages(16, 0, 4, size, 3);
        pages.push(page((4, 4), 1, 8, |_, _, _| 1));
        write_tiff(&source, &pages, Some(&xml));
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        let state = wait_ready(&backend);
        assert!(state.rgb && state.sample_bits == 16, "{state:?}");
        assert_eq!(
            state
                .channels
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["Red", "Green", "Blue"]
        );
        assert_eq!(state.skipped_series, 1);
        assert_eq!(state.dimensions.as_ref().unwrap().positions, ["RGB"]);
        for (z, time) in [(1, 0), (0, 1), (1, 1)] {
            select(&backend, z, time, None);
            for band in 0..3 {
                assert_eq!(
                    samples(
                        &backend,
                        Asset::ChannelPreview {
                            channel: band.into()
                        }
                    ),
                    expected(16, 0, z + 2 * time, size, band)
                );
            }
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unmapped_planes_fail_only_their_selection() {
        if !vips() {
            return;
        }
        let root = root("unmapped");
        let size = (8, 5);
        let xml = ome(
            "urn:uuid:a",
            &image(
                "A",
                "uint8",
                size,
                [1, 3, 1],
                "",
                r#"<TiffData IFD="0"/><TiffData IFD="1" FirstZ="2"/>"#,
            ),
        );
        let source = root.join("gaps.ome.tif");
        write_tiff(&source, &plane_pages(8, 0, 2, size, 1), Some(&xml));
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&source).unwrap();
        wait_ready(&backend);
        let failed = select(&backend, 1, 0, None);
        assert_eq!(failed.phase, Phase::Error);
        assert_eq!(
            failed.error.as_deref(),
            Some("OME-TIFF plane C 1, Z 2, T 1 is not stored in this dataset.")
        );
        // Navigation stays available from the failed selection.
        assert_eq!(failed.dimensions.as_ref().unwrap().z_index, 1);
        assert_eq!(select(&backend, 2, 0, None).phase, Phase::Ready);
        assert_eq!(
            samples(&backend, Asset::ChannelPreview { channel: 0 }),
            expected(8, 0, 1, size, 0)
        );
        // A projection over an unmapped plane fails explicitly.
        let projected = select(&backend, 0, 0, Some(Projection::Z));
        assert_eq!(projected.phase, Phase::Error);
        assert!(projected
            .error
            .unwrap()
            .contains("Z 2, T 1 in the projected range is not stored"));
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    fn pyramid_page(member: u32, ifd: u32, size: (u32, u32), factor: u32) -> Page {
        let mut base = page(size, 1, 16, |x, y, band| value(16, member, ifd, x, y, band));
        base.tile = Some(16);
        let mut level = size;
        for _ in 0..2 {
            level = (level.0.div_ceil(factor), level.1.div_ceil(factor));
            let mut reduced = page(level, 1, 16, |x, y, band| {
                value(16, member, ifd, x * 2, y * 2, band)
            });
            reduced.tile = Some(16);
            base.subifds.push(reduced);
        }
        base
    }

    #[test]
    fn subifd_pyramids_are_reused_per_plane_only_for_power_of_two_levels() {
        if !vips() {
            return;
        }
        let root = root("pyramid");
        // Factor-3 levels of this size are not power-of-two reductions.
        let size = (90, 54);
        let backend = Backend::new(root.join("cache")).unwrap();
        for factor in [2, 3] {
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "P",
                    "uint16",
                    size,
                    [2, 2, 1],
                    "",
                    r#"<TiffData PlaneCount="4"/>"#,
                ),
            );
            let source = root.join(format!("pyramid-{factor}.ome.tif"));
            let pages: Vec<_> = (0..4)
                .map(|ifd| pyramid_page(0, ifd, size, factor))
                .collect();
            write_tiff(&source, &pages, Some(&xml));
            backend.open(&source).unwrap();
            wait_ready(&backend);
            let state = select(&backend, 1, 0, None);
            assert_eq!(state.phase, Phase::Ready, "{state:?}");
            let job = backend.current.lock().unwrap().clone().unwrap();
            let native = job.native.lock().unwrap().clone();
            let tiles = job.dir.lock().unwrap().join("channels/1/tiles_files");
            if factor == 2 {
                assert_eq!(native.len(), 3, "{native:?}");
                assert!(!tiles.exists());
            } else {
                assert!(native.is_empty());
                assert!(tiles.is_dir());
            }
            // Full-resolution detail of channel 1 at Z 2 is page 3 either way.
            let level = max_level(size.0, size.1);
            let tile = samples(
                &backend,
                Asset::ChannelTile {
                    channel: 1,
                    level,
                    x: 0,
                    y: 0,
                },
            );
            assert_eq!(tile, expected(16, 0, 3, size, 0));
            assert_eq!(state.sample_bits, 16);
        }
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_secondary_member_fails_reselection_without_stale_reuse() {
        if !vips() {
            return;
        }
        let root = root("changed");
        let size = (6, 4);
        let data = r#"<TiffData PlaneCount="1"><UUID FileName="a.ome.tif">urn:uuid:a</UUID></TiffData><TiffData FirstZ="1" PlaneCount="1"><UUID FileName="b.tif">urn:uuid:b</UUID></TiffData>"#;
        let a = root.join("a.ome.tif");
        let b = root.join("b.tif");
        write_tiff(
            &a,
            &plane_pages(16, 0, 1, size, 1),
            Some(&ome(
                "urn:uuid:a",
                &image("S", "uint16", size, [1, 2, 1], "", data),
            )),
        );
        write_tiff(&b, &plane_pages(16, 1, 1, size, 1), None);
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&a).unwrap();
        wait_ready(&backend);
        assert_eq!(select(&backend, 1, 0, None).phase, Phase::Ready);
        assert_eq!(
            samples(&backend, Asset::ChannelPreview { channel: 0 }),
            expected(16, 1, 0, size, 0)
        );
        let cached = backend.current.lock().unwrap().clone().unwrap().cache_key();
        assert!(root.join("cache").join(&cached).join("complete").is_file());
        select(&backend, 0, 0, None);
        // Replace the secondary member with different pixels.
        let replacement = root.join("replacement.tif");
        write_tiff(&replacement, &plane_pages(16, 9, 1, size, 1), None);
        fs::rename(&replacement, &b).unwrap();
        let failed = select(&backend, 1, 0, None);
        assert_eq!(failed.phase, Phase::Error);
        let error = failed.error.unwrap();
        assert!(
            error.contains("changed on disk; reopen") && error.contains("b.tif"),
            "{error}"
        );
        assert!(!failed.preview_ready);
        // Reopening reads the new member identity and pixels.
        backend.open(&a).unwrap();
        wait_ready(&backend);
        select(&backend, 1, 0, None);
        assert_eq!(
            samples(&backend, Asset::ChannelPreview { channel: 0 }),
            expected(16, 9, 0, size, 0)
        );
        assert_ne!(
            backend.current.lock().unwrap().clone().unwrap().cache_key(),
            cached
        );
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_tiles_check_their_member_before_reading_and_publishing() {
        if !vips() {
            return;
        }
        let root = root("native-member");
        let size = (90, 54);
        let data = r#"<TiffData PlaneCount="1"><UUID FileName="a.ome.tif">urn:uuid:a</UUID></TiffData><TiffData FirstZ="1" PlaneCount="1"><UUID FileName="b.tif">urn:uuid:b</UUID></TiffData>"#;
        let a = root.join("a.ome.tif");
        let b = root.join("b.tif");
        write_tiff(
            &a,
            &[pyramid_page(0, 0, size, 2)],
            Some(&ome(
                "urn:uuid:a",
                &image("S", "uint16", size, [1, 2, 1], "", data),
            )),
        );
        write_tiff(&b, &[pyramid_page(1, 0, size, 2)], None);
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&a).unwrap();
        wait_ready(&backend);
        assert_eq!(select(&backend, 1, 0, None).phase, Phase::Ready);
        let job = backend.current.lock().unwrap().clone().unwrap();
        assert!(job.native.lock().unwrap().len() > 1);
        let level = max_level(size.0, size.1);
        let tile = |level| Asset::ChannelTile {
            channel: 0,
            level,
            x: 0,
            y: 0,
        };
        assert_eq!(samples(&backend, tile(level)), expected(16, 1, 0, size, 0));
        let replacement = root.join("replacement.tif");
        write_tiff(&replacement, &[pyramid_page(9, 0, size, 2)], None);
        fs::rename(&replacement, &b).unwrap();
        let (status, error) = read_asset(&job, tile(level - 1)).unwrap_err();
        assert_eq!(status, 500);
        assert!(
            error.contains("changed on disk") && error.contains("b.tif"),
            "{error}"
        );
        let published = job.dir.lock().unwrap().join("channels/0/native_tiles");
        assert!(!published.join(format!("{}-0-0.png", level - 1)).exists());
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_companion_metadata_changes_the_cache_key() {
        if !vips() {
            return;
        }
        let root = root("companion-key");
        let size = (8, 6);
        let data = root.join("data.ome.tif");
        let companion = root.join("set.companion.ome");
        write_tiff(
            &data,
            &plane_pages(16, 0, 2, size, 1),
            Some(&ome(
                "urn:uuid:data",
                r#"<BinaryOnly MetadataFile="set.companion.ome" UUID="urn:uuid:meta"/>"#,
            )),
        );
        let metadata = |name: &str| {
            ome(
                "urn:uuid:meta",
                &image(
                    "Set",
                    "uint16",
                    size,
                    [1, 2, 1],
                    &format!(r#"<Channel Name="{name}"/>"#),
                    r#"<TiffData PlaneCount="2"><UUID FileName="data.ome.tif">urn:uuid:data</UUID></TiffData>"#,
                ),
            )
        };
        fs::write(&companion, metadata("Before")).unwrap();
        let original = fs::read(&data).unwrap();
        let backend = Backend::new(root.join("cache")).unwrap();
        backend.open(&data).unwrap();
        let first = wait_ready(&backend);
        assert_eq!(first.channels[0].name, "Before");
        let key = backend.current.lock().unwrap().clone().unwrap().cache_key();
        let replacement = root.join("replacement.ome");
        fs::write(&replacement, metadata("After")).unwrap();
        fs::rename(&replacement, &companion).unwrap();
        // The open dataset notices its changed metadata file.
        let failed = select(&backend, 1, 0, None);
        let error = failed.error.unwrap();
        assert!(
            error.contains("changed on disk") && error.contains("set.companion.ome"),
            "{error}"
        );
        // Reopening the unchanged binary member reads the new metadata.
        backend.open(&data).unwrap();
        let reopened = wait_ready(&backend);
        assert_eq!(reopened.channels[0].name, "After");
        assert!(!reopened.detail.contains("Reused"), "{reopened:?}");
        assert_ne!(
            backend.current.lock().unwrap().clone().unwrap().cache_key(),
            key
        );
        assert_eq!(fs::read(&data).unwrap(), original);
        backend.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projections_take_exact_maxima_of_original_samples() {
        if !vips() {
            return;
        }
        let root = root("projection");
        let size = (13, 9);
        for bits in [8u16, 16] {
            let kind = if bits == 8 { "uint8" } else { "uint16" };
            let xml = ome(
                "urn:uuid:a",
                &image(
                    "A",
                    kind,
                    size,
                    [2, 3, 2],
                    "",
                    r#"<TiffData PlaneCount="12"/>"#,
                ),
            );
            let source = root.join(format!("projection-{bits}.ome.tif"));
            // Pages alternate between rising and falling ramps, so maxima mix planes.
            let ramp = |ifd: u32, x: u32, y: u32| -> u16 {
                let v = if ifd.is_multiple_of(2) {
                    x * 300 + y * 7 + ifd
                } else {
                    (12 - x) * 300 + y * 5 + ifd
                };
                if bits == 8 {
                    (v % 256) as u16
                } else {
                    (v + 1000 * ifd) as u16
                }
            };
            let pages: Vec<_> = (0..12)
                .map(|ifd| page(size, 1, bits, |x, y, _| ramp(ifd, x, y)))
                .collect();
            write_tiff(&source, &pages, Some(&xml));
            let backend = Backend::new(root.join(format!("cache-{bits}"))).unwrap();
            backend.open(&source).unwrap();
            wait_ready(&backend);
            for (projection, z, time) in [
                (Projection::Z, 0..3, 1..2),
                (Projection::Time, 2..3, 0..2),
                (Projection::ZTime, 0..3, 0..2),
            ] {
                let state = select(&backend, z.start, time.start, Some(projection));
                assert_eq!(state.phase, Phase::Ready, "{state:?}");
                assert_eq!(state.projection, projection);
                for c in 0..2 {
                    let maxima: Vec<u16> = (0..size.1)
                        .flat_map(|y| (0..size.0).map(move |x| (x, y)))
                        .map(|(x, y)| {
                            time.clone()
                                .flat_map(|t| z.clone().map(move |z| c + 2 * (z + 3 * t)))
                                .map(|ifd| ramp(ifd, x, y))
                                .max()
                                .unwrap()
                        })
                        .collect();
                    assert_eq!(
                        samples(&backend, Asset::ChannelPreview { channel: c }),
                        maxima
                    );
                    if bits == 16 {
                        let range = state.display_ranges[c as usize];
                        assert_eq!(
                            range,
                            [*maxima.iter().min().unwrap(), *maxima.iter().max().unwrap()]
                        );
                    }
                }
            }
            assert_eq!(
                select(&backend, 1, 1, Some(Projection::None)).phase,
                Phase::Ready
            );
            backend.shutdown();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn page_reduction_validates_layout_and_stops_on_cancel() {
        let root = root("reduce");
        let path = root.join("page.v");
        // A libvips little-endian header followed by 3 × 2 ushort samples.
        let header = |width: u32, format: u32| {
            let mut bytes = vec![0u8; 64];
            bytes[..4].copy_from_slice(&[0xb6, 0xa6, 0xf2, 0x08]);
            bytes[4..8].copy_from_slice(&width.to_le_bytes());
            bytes[8..12].copy_from_slice(&2u32.to_le_bytes());
            bytes[12..16].copy_from_slice(&1u32.to_le_bytes());
            bytes[20..24].copy_from_slice(&format.to_le_bytes());
            bytes
        };
        let body: Vec<u8> = [5u16, 600, 7, 8, 65535, 1]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        fs::write(&path, [header(3, 2), body.clone()].concat()).unwrap();
        let mut accumulator: Vec<u8> = [9u16, 9, 1000, 1000, 9, 9, 9, 9, 9, 9, 9, 9]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let cancel = AtomicBool::new(false);
        reduce_page(&path, &mut accumulator, (3, 2, 2, 2), Some(1), &cancel).unwrap();
        let values: Vec<u16> = accumulator
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_le_bytes(*v))
            .collect();
        assert_eq!(values, [9, 9, 1000, 1000, 9, 9, 9, 9, 9, 65535, 9, 9]);
        assert!(
            reduce_page(&path, &mut accumulator, (4, 2, 2, 2), Some(1), &cancel)
                .unwrap_err()
                .contains("differs")
        );
        fs::write(&path, [header(3, 0), body].concat()).unwrap();
        assert!(reduce_page(&path, &mut accumulator, (3, 2, 2, 2), Some(1), &cancel).is_err());
        cancel.store(true, Ordering::Relaxed);
        let samples = CANCEL_CHUNK_SAMPLES + 1;
        let mut big = header(samples as u32, 2);
        big[8..12].copy_from_slice(&1u32.to_le_bytes());
        big.extend(vec![0xff; samples * 2]);
        fs::write(&path, big).unwrap();
        let mut accumulator = vec![0; samples * 2];
        assert!(reduce_page(
            &path,
            &mut accumulator,
            (samples as u32, 1, 1, 2),
            Some(0),
            &cancel
        )
        .unwrap_err()
        .contains("cancelled"));
        assert!(accumulator.iter().all(|v| *v == 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn projection_admission_and_large_frame_limits() {
        if !vips() {
            return;
        }
        let root = root("admission");
        let size = (64, 64);
        let xml = ome(
            "urn:uuid:a",
            &image(
                "A",
                "uint16",
                size,
                [1, 2, 1],
                "",
                r#"<TiffData PlaneCount="2"/>"#,
            ),
        );
        let source = root.join("admission.ome.tif");
        write_tiff(&source, &plane_pages(16, 0, 2, size, 1), Some(&xml));
        let backend = Backend::new(root.join("cache")).unwrap();
        backend
            .memory
            .set_limit(crate::memory::MIN_LIMIT_BYTES)
            .unwrap();
        backend.open(&source).unwrap();
        wait_ready(&backend);
        // Optional preloading changes available memory; let it settle, then
        // leave less than the accumulator, two pages and the read chunk.
        let settled = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while backend.preload_status().running && std::time::Instant::now() < settled {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let full = loop {
            let available = backend.memory.status().unwrap().available_bytes;
            if let Some(full) = backend.memory.try_reserve(available - 1024, false).unwrap() {
                break full;
            }
        };
        let failed = select(&backend, 0, 0, Some(Projection::Z));
        assert_eq!(failed.phase, Phase::Error);
        assert!(
            failed.error.unwrap().contains("does not fit"),
            "{:?}",
            backend.snapshot()
        );
        drop(full);
        assert_eq!(
            select(&backend, 0, 0, Some(Projection::Z)).phase,
            Phase::Ready
        );
        backend.shutdown();
        // Frames above 256 MiB browse plane by plane but cannot be projected.
        let series = Series {
            name: "Large".into(),
            width: 16384,
            height: 8193,
            size_z: 2,
            size_t: 1,
            channels: vec![crate::channels::Channel {
                name: "A".into(),
                color: "#ffffff".into(),
                domain: [0, 65535],
            }],
            rgb: false,
            sample_bits: 16,
            calibration: crate::frames::Calibration {
                pixel_width_um: None,
                pixel_height_um: None,
                z_step_um: None,
                time_increment_seconds: None,
            },
            planes: Vec::new(),
        };
        assert!(projection_reason(&series).unwrap().contains("256 MiB"));
        let fits = Series {
            height: 8192,
            ..series
        };
        assert!(projection_reason(&fits).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
