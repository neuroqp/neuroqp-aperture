//! Image-scoped libvips worker. All native handles stay on this process's main
//! thread; killing the process cancels decoding without sharing C state with UI.
use crate::channels::Plane;
use libvips::{ops, VipsApp, VipsImage, VipsInterpolate};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufRead, Read, Write},
    path::Path,
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    TiffChannelInfo {
        input: String,
    },
    Command {
        program: String,
        args: Vec<String>,
    },
    ChannelPreviews {
        input: String,
        options: Vec<String>,
        scale: Vec<f64>,
        planes: Vec<Plane>,
        ushort: bool,
        directory: String,
    },
    ChannelPyramid {
        input: String,
        output: String,
        band: u32,
        ushort: bool,
    },
    RawPreview {
        input: String,
        output: String,
        max_edge: u32,
        threads: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Response {
    Progress { progress: f64 },
    Complete { result: Result<String, String> },
}

struct Progress {
    last: Mutex<Option<(u8, Instant)>>,
}

impl Progress {
    fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    fn update(&self, fraction: f64) {
        if let Some(progress) = self.accept(fraction, Instant::now()) {
            // Never hold stdout while evaluating a native image or raw strip.
            let _ = write_response(&Response::Progress { progress });
        }
    }

    fn accept(&self, fraction: f64, now: Instant) -> Option<f64> {
        if !fraction.is_finite() {
            return None;
        }
        let bucket = (fraction.clamp(0.0, 1.0) * 10.0).floor() as u8;
        let mut last = self.last.lock().ok()?;
        if let Some((previous, at)) = *last {
            if bucket <= previous
                || (bucket < 10 && now.duration_since(at) < Duration::from_millis(250))
            {
                return None;
            }
        }
        *last = Some((bucket, now));
        Some(f64::from(bucket) / 10.0)
    }
}

struct EvalData<'a> {
    progress: &'a Progress,
    start: f64,
    span: f64,
}

struct EvalProgress<'a> {
    signal: u32,
    hook: std::os::raw::c_ulong,
    // Stable allocation remains alive until the emission hook is removed.
    _data: Box<EvalData<'a>>,
}

impl<'a> EvalProgress<'a> {
    fn new(progress: &'a Progress, start: f64, span: f64) -> Result<Self, String> {
        use libvips::bindings;
        let mut data = Box::new(EvalData {
            progress,
            start,
            span,
        });
        // The public GLib emission hook avoids reading the binding's private
        // native-pointer field. This worker evaluates exactly one sink at a time.
        let signal =
            unsafe { bindings::g_signal_lookup(c"eval".as_ptr(), bindings::vips_image_get_type()) };
        if signal == 0 {
            return Err("libvips evaluation progress signal is unavailable.".into());
        }
        let hook = unsafe {
            bindings::g_signal_add_emission_hook(
                signal,
                0,
                Some(eval_progress),
                (&mut *data as *mut EvalData<'a>).cast(),
                None,
            )
        };
        if hook == 0 {
            return Err("Cannot observe libvips evaluation progress.".into());
        }
        Ok(Self {
            signal,
            hook,
            _data: data,
        })
    }
}

impl Drop for EvalProgress<'_> {
    fn drop(&mut self) {
        unsafe { libvips::bindings::g_signal_remove_emission_hook(self.signal, self.hook) };
    }
}

unsafe extern "C" fn eval_progress(
    _: *mut libvips::bindings::GSignalInvocationHint,
    count: u32,
    values: *const libvips::bindings::GValue,
    data: *mut std::ffi::c_void,
) -> i32 {
    if count >= 2 && !values.is_null() && !data.is_null() {
        let native = libvips::bindings::g_value_get_pointer(values.add(1))
            .cast::<libvips::bindings::VipsProgress>();
        if !native.is_null() {
            let data = &*data.cast::<EvalData<'_>>();
            // Completion belongs to the whole request, after its sink succeeds.
            let fraction = f64::from((*native).percent).clamp(0.0, 100.0) / 100.0;
            data.progress
                .update((data.start + data.span * fraction).min(0.99));
        }
    }
    1
}

fn evaluate<T>(
    image: &VipsImage,
    progress: &Progress,
    start: f64,
    span: f64,
    sink: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _hook = EvalProgress::new(progress, start, span)?;
    image.image_set_progress(true);
    let result = sink();
    image.image_set_progress(false);
    result
}

fn save_image(
    image: &VipsImage,
    output: &str,
    progress: &Progress,
    start: f64,
    span: f64,
) -> Result<(), String> {
    evaluate(image, progress, start, span, || {
        image.image_write_to_file(output).map_err(|e| e.to_string())
    })
}

fn write_response(response: &Response) -> Result<(), String> {
    let response = serde_json::to_vec(response).map_err(|e| e.to_string())?;
    if response.len() >= MAX_MESSAGE {
        return Err("Image worker response exceeds its size limit.".into());
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&response)
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}

struct Worker {
    // Never VipsImage::clone(): libvips 2.3 derives a shallow native-pointer clone.
    images: HashMap<String, Rc<VipsImage>>,
    source: Option<String>,
}

impl Worker {
    fn open(&mut self, input: &str) -> Result<Rc<VipsImage>, String> {
        if let Some(image) = self.images.get(input) {
            return Ok(Rc::clone(image));
        }
        let image = Rc::new(crate::platform::open_image(input)?);
        // Parent supplies a safe job-local source alias. Scratch outputs must
        // never stay open after their individual operation has finished.
        let path = input.split('[').next().unwrap_or(input);
        if self.source.is_none() {
            self.source = Some(path.into());
        }
        if self.source.as_deref() != Some(path) {
            return Ok(image);
        }
        // Bound retained header/loader handles, independently of libvips' cache.
        if self.images.len() >= 64 {
            self.images.clear();
        }
        self.images.insert(input.into(), Rc::clone(&image));
        Ok(image)
    }

    fn process(&mut self, request: Request) -> Result<String, String> {
        let progress = Progress::new();
        progress.update(0.0);
        let result = self.process_with_progress(request, &progress);
        if result.as_ref().is_ok_and(|value| value != "unsupported") {
            progress.update(1.0);
        }
        result
    }

    fn process_with_progress(
        &mut self,
        request: Request,
        progress: &Progress,
    ) -> Result<String, String> {
        match request {
            Request::TiffChannelInfo { input } => {
                let path =
                    crate::platform::source_path(Path::new(&input)).map_err(|e| e.to_string())?;
                crate::tiff_annotations::channel_info(&path)
            }
            Request::Command { program, args } => {
                self.command_with_progress(&program, &args, progress)
            }
            Request::ChannelPreviews {
                input,
                options,
                scale,
                planes,
                ushort,
                directory,
            } => {
                self.channel_previews(
                    &input,
                    &options,
                    &scale,
                    &planes,
                    ushort,
                    (&directory, progress),
                )?;
                Ok(String::new())
            }
            Request::ChannelPyramid {
                input,
                output,
                band,
                ushort,
            } => {
                let result = (|| {
                    let source = self.open(&input)?;
                    let band = i32::try_from(band).map_err(|_| "Invalid image band.")?;
                    let extracted = ops::extract_band(&source, band).map_err(|e| e.to_string())?;
                    // Keep extraction lazy, so one source traversal feeds the
                    // lossless pyramid without full decoded planes in memory or
                    // intermediate disk files.
                    save_pyramid(
                        &display_plane(&extracted, ushort)?,
                        &output,
                        &format!(
                            "tile-size=256,overlap=0,{},depth=onepixel",
                            crate::backend::pyramid::channel_suffix(ushort)
                        ),
                        progress,
                    )
                })();
                if input.contains("access=sequential") {
                    self.images.remove(&input);
                }
                result.map(|_| String::new())
            }
            Request::RawPreview {
                input,
                output,
                max_edge,
                threads,
            } => {
                if max_edge == 0 || max_edge > 1024 || threads == 0 || threads > 16 {
                    return Err("Invalid raw TIFF preview limits.".into());
                }
                let Some(preview) = crate::raw_tiff::preview_with_progress(
                    &crate::platform::source_path(Path::new(&input)).map_err(|e| e.to_string())?,
                    max_edge,
                    threads,
                    &|fraction| progress.update(fraction * 0.98),
                )
                .map_err(|e| e.to_string())?
                else {
                    return Ok("unsupported".into());
                };
                let (format, interpretation) = match (preview.bits, preview.bands) {
                    (8, 1) => (ops::BandFormat::Uchar, ops::Interpretation::BW),
                    (8, 3) => (ops::BandFormat::Uchar, ops::Interpretation::Srgb),
                    (16, 1) => (ops::BandFormat::Ushort, ops::Interpretation::Grey16),
                    (16, 3) => (ops::BandFormat::Ushort, ops::Interpretation::Rgb16),
                    _ => return Err("Invalid raw TIFF preview format.".into()),
                };
                let image = VipsImage::new_from_memory_copy(
                    &preview.pixels,
                    preview.width as i32,
                    preview.height as i32,
                    preview.bands as i32,
                    format,
                )
                .map_err(|e| e.to_string())?;
                // A PNG output is a lossless single-channel display plane.
                let options = if output.ends_with(".png") {
                    if preview.bands != 1 {
                        return Err("Raw TIFF channel previews must have one band.".into());
                    }
                    "[strip]"
                } else {
                    "[Q=85,strip,background=52 54 51]"
                };
                save_image(
                    &reinterpret(&image, interpretation)?,
                    &format!("{output}{options}"),
                    progress,
                    0.98,
                    0.02,
                )?;
                Ok("used".into())
            }
        }
    }

    #[cfg(test)]
    fn command(&mut self, program: &str, args: &[String]) -> Result<String, String> {
        self.command_with_progress(program, args, &Progress::new())
    }

    fn command_with_progress(
        &mut self,
        program: &str,
        args: &[String],
        progress: &Progress,
    ) -> Result<String, String> {
        if program == "vipsheader" {
            if args.len() != 2 || args[0] != "-a" {
                return Err("Unsupported metadata request.".into());
            }
            let image = self.open(&args[1])?;
            return header(&image);
        }
        if program != "vips" || args.len() < 3 {
            return Err("Unsupported libvips request.".into());
        }
        let image = self.open(&args[1])?;
        if args[0] == "getpoint" && args.len() == 4 {
            let x = args[2]
                .parse::<i32>()
                .map_err(|_| "Invalid point coordinate.")?;
            let y = args[3]
                .parse::<i32>()
                .map_err(|_| "Invalid point coordinate.")?;
            let pixel = ops::extract_area(&image, x, y, 1, 1)
                .and_then(|i| ops::cast(&i, ops::BandFormat::Double))
                .map_err(|e| e.to_string())?;
            let memory = evaluate(&pixel, progress, 0.0, 1.0, || {
                Ok(pixel.image_write_to_memory())
            })?;
            let values = memory
                .as_chunks::<8>()
                .0
                .iter()
                .map(|sample| f64::from_ne_bytes(*sample).to_string())
                .collect::<Vec<_>>();
            return Ok(format!("{}\n", values.join(" ")));
        }
        let output = &args[2];
        let result = match args[0].as_str() {
            "resize" if args.len() == 4 => {
                let scale = positive(&args[3])?;
                let resized = ops::resize(&image, scale).map_err(|e| e.to_string())?;
                save_image(&resized, output, progress, 0.0, 1.0)
            }
            "extract_band" if args.len() == 5 && args[4] == "--n=1" => {
                let band = args[3].parse::<i32>().map_err(|_| "Invalid image band.")?;
                let extracted = ops::extract_band(&image, band).map_err(|e| e.to_string())?;
                save_image(&extracted, output, progress, 0.0, 1.0)
            }
            "tiffsave" if args.len() == 4 && args[3] == "--compression=lzw" => {
                ops::tiffsave_with_opts(
                    &image,
                    output,
                    &ops::TiffsaveOptions {
                        compression: ops::ForeignTiffCompression::Lzw,
                        ..Default::default()
                    },
                )
                .map_err(|e| e.to_string())
            }
            "copy" if args.len() == 3 => save_image(&image, output, progress, 0.0, 1.0),
            "copy"
                if args.len() == 4
                    && matches!(
                        args[3].as_str(),
                        "--interpretation=b-w" | "--interpretation=grey16"
                    ) =>
            {
                let ushort = args[3] == "--interpretation=grey16";
                save_image(&display_plane(&image, ushort)?, output, progress, 0.0, 1.0)
            }
            "dzsave" => {
                let options = dz_options(&args[3..])?;
                save_pyramid(&image, output, &options, progress)
            }
            "affine"
                if args.len() == 7
                    && args[5] == "--interpolate=bilinear"
                    && args[6] == "--extend=copy" =>
            {
                let matrix = numbers::<f64>(&args[3], 4)?;
                if matrix.iter().any(|v| !v.is_finite()) {
                    return Err("Invalid affine matrix.".into());
                }
                let area = numbers::<i32>(
                    args[4]
                        .strip_prefix("--oarea=")
                        .ok_or("Invalid output area.")?,
                    4,
                )?;
                if area[2] <= 0 || area[3] <= 0 {
                    return Err("Invalid output area.".into());
                }
                let settings = ops::AffineOptions {
                    interpolate: VipsInterpolate::new_from_name("bilinear")
                        .map_err(|e| e.to_string())?,
                    oarea: area,
                    extend: ops::Extend::Copy,
                    odx: 0.0,
                    ody: 0.0,
                    idx: 0.0,
                    idy: 0.0,
                    background: vec![0.0],
                    premultiplied: false,
                };
                let transformed = ops::affine_with_opts(
                    &image, matrix[0], matrix[1], matrix[2], matrix[3], &settings,
                )
                .map_err(|e| e.to_string())?;
                save_image(&transformed, output, progress, 0.0, 1.0)
            }
            _ => Err(format!("Unsupported libvips operation: {}.", args[0])),
        };
        // A sequential loader can be exhausted after one sink. Metadata/random
        // sources remain reusable, but subsequent sequential sinks reopen safely.
        if args[1].contains("access=sequential") {
            self.images.remove(&args[1]);
        }
        result.map(|_| String::new())
    }

    fn channel_previews(
        &mut self,
        input: &str,
        options: &[String],
        scales: &[f64],
        planes: &[Plane],
        ushort: bool,
        (directory, progress): (&str, &Progress),
    ) -> Result<(), String> {
        if planes.is_empty()
            || planes.len() > 32
            || options.len() != planes.len()
            || scales.len() != planes.len()
        {
            return Err("Invalid channel preview request.".into());
        }
        let mut resized: HashMap<(String, u64), VipsImage> = HashMap::new();
        for (index, plane) in planes.iter().enumerate() {
            let scale = scales[index];
            if !scale.is_finite() || scale <= 0.0 || scale > 1.0 {
                return Err("Invalid preview scale.".into());
            }
            let key = (options[index].clone(), scale.to_bits());
            if !resized.contains_key(&key) {
                let source_name = format!("{input}{}", options[index]);
                let source = self.open(&source_name)?;
                let small = ops::resize(&source, scale).map_err(|e| e.to_string())?;
                let bytes = u64::try_from(small.get_width())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(u64::try_from(small.get_height()).unwrap_or(u64::MAX))
                    .saturating_mul(u64::try_from(small.get_bands()).unwrap_or(u64::MAX))
                    .saturating_mul(if ushort { 2 } else { 1 });
                if small.get_width().max(small.get_height()) > 1024 || bytes > 64 * 1024 * 1024 {
                    return Err("Channel preview exceeds its memory limit.".into());
                }
                // Evaluate once before splitting bands. Multiple PNG sinks must
                // never traverse the full TIFF source again with cache disabled.
                // This temporary image is consumed by copy_memory and never retained.
                let hook = EvalProgress::new(
                    progress,
                    index as f64 / planes.len() as f64,
                    0.9 / planes.len() as f64,
                )?;
                small.image_set_progress(true);
                let memory = VipsImage::image_copy_memory(small).map_err(|e| e.to_string())?;
                drop(hook);
                resized.insert(key.clone(), memory);
                self.images.remove(&source_name);
            }
            let band = i32::try_from(plane.band).map_err(|_| "Invalid channel band.")?;
            let extracted = ops::extract_band(&resized[&key], band).map_err(|e| e.to_string())?;
            let display = display_plane(&extracted, ushort)?;
            let channel = Path::new(directory)
                .join("channels")
                .join(index.to_string());
            fs::create_dir_all(&channel).map_err(|e| e.to_string())?;
            let partial = channel.join("preview.partial.png");
            save_image(
                &display,
                partial.to_str().ok_or("Invalid preview path.")?,
                progress,
                (index as f64 + 0.9) / planes.len() as f64,
                0.1 / planes.len() as f64,
            )?;
            fs::rename(partial, channel.join("preview.png")).map_err(|e| e.to_string())?;
            progress.update(((index + 1) as f64 / planes.len() as f64).min(0.99));
            // Each page needs at most one <=1024px image. Release pages as soon
            // as their last band is written instead of retaining all 32 pages.
            if !(index + 1..planes.len())
                .any(|next| options[next] == key.0 && scales[next].to_bits() == key.1)
            {
                resized.remove(&key);
            }
        }
        Ok(())
    }
}

/// Label one band as a lossless display plane; PNG saves grey16 at 16 bits.
fn display_plane(image: &VipsImage, ushort: bool) -> Result<VipsImage, String> {
    reinterpret(
        image,
        if ushort {
            ops::Interpretation::Grey16
        } else {
            ops::Interpretation::BW
        },
    )
}

fn reinterpret(
    image: &VipsImage,
    interpretation: ops::Interpretation,
) -> Result<VipsImage, String> {
    // libvips 2.3.0's get_coding() incorrectly calls get_format(). Read the
    // actual metadata so ushort pixels are never mislabeled as LabQ coding.
    let coding = match enum_name(
        &image.get_as_string("coding").map_err(|e| e.to_string())?,
        "VIPS_CODING_",
    )
    .as_str()
    {
        "none" => ops::Coding::None,
        "labq" => ops::Coding::Labq,
        "rad" => ops::Coding::Rad,
        "error" => ops::Coding::Error,
        _ => return Err("Unknown image coding.".into()),
    };
    // Binding options set every field, so preserve the existing header explicitly.
    let settings = ops::CopyOptions {
        width: image.get_width(),
        height: image.get_height(),
        bands: image.get_bands(),
        format: image.get_format().map_err(|e| e.to_string())?,
        coding,
        interpretation,
        xres: image
            .get_as_string("xres")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0),
        yres: image
            .get_as_string("yres")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0),
        xoffset: image.get_xoffset(),
        yoffset: image.get_yoffset(),
    };
    ops::copy_with_opts(image, &settings).map_err(|e| e.to_string())
}

fn header(image: &VipsImage) -> Result<String, String> {
    let mut output = String::new();
    for key in [
        "width",
        "height",
        "bands",
        "format",
        "coding",
        "interpretation",
        "orientation",
        "vips-loader",
        "magick-format",
        "n-pages",
        "n-subifds",
        "tile-width",
        "tile-height",
        "image-description",
    ] {
        if let Ok(value) = image.get_as_string(key) {
            // vipsheader prints enum nicknames, while get_as_string prints the
            // GValue type and symbol. Existing layout validation needs nicknames.
            let value = match key {
                "vips-loader" => value.strip_suffix("_source").unwrap_or(&value).to_owned(),
                "format" => enum_name(&value, "VIPS_FORMAT_"),
                "coding" => enum_name(&value, "VIPS_CODING_"),
                "interpretation" => enum_name(&value, "VIPS_INTERPRETATION_"),
                _ => value,
            };
            output.push_str(&format!("{key}: {value}\n"));
        }
        if output.len() > MAX_MESSAGE / 2 {
            return Err("Image metadata exceeds its size limit.".into());
        }
    }
    Ok(output)
}

fn enum_name(value: &str, prefix: &str) -> String {
    value.split_once(prefix).map_or_else(
        || value.to_string(),
        |(_, name)| {
            name.trim_end_matches(')')
                .to_ascii_lowercase()
                .replace('_', "-")
        },
    )
}

fn positive(value: &str) -> Result<f64, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| "Invalid image scale.".into())
}

fn numbers<T: std::str::FromStr>(value: &str, length: usize) -> Result<Vec<T>, String> {
    let values = value
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<T>, _>>()
        .map_err(|_| "Invalid numeric argument.")?;
    if values.len() != length {
        return Err("Invalid numeric argument length.".into());
    }
    Ok(values)
}

fn save_pyramid(
    image: &VipsImage,
    output: &str,
    options: &str,
    progress: &Progress,
) -> Result<(), String> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8) as i32;
    // Requests run serially in this isolated worker. Raise the global libvips
    // budget only for the pyramid sink, then restore preview/native-tile limits.
    let previous = unsafe { libvips::bindings::vips_concurrency_get() };
    unsafe { libvips::bindings::vips_concurrency_set(threads) };
    let result = save_image(
        image,
        &format!("{output}.dz[container=fs,{options}]"),
        progress,
        0.0,
        1.0,
    );
    unsafe { libvips::bindings::vips_concurrency_set(previous) };
    result
}

fn dz_options(args: &[String]) -> Result<String, String> {
    let mut options = Vec::new();
    for arg in args {
        let option = arg.strip_prefix("--").ok_or("Invalid pyramid option.")?;
        if !matches!(
            option,
            "tile-size=256"
                | "overlap=0"
                | "depth=onepixel"
                | "background=52 54 51"
                | "suffix=.png[strip]"
                | "suffix=.png[strip,compression=1]"
                | "suffix=.jpg[Q=85,strip,background=52 54 51]"
                | "Q=85"
        ) {
            return Err("Unsupported pyramid option.".into());
        }
        options.push(option);
    }
    Ok(options.join(","))
}

/// libvips limits shared by the image worker and the vips command-line tools.
pub(crate) const VIPS_THREADS: i32 = 2;
const VIPS_CACHE_MAX_MEMORY: u64 = 64 * 1024 * 1024;
const VIPS_DISC_THRESHOLD: &str = "32m";

pub(crate) fn tool_options() -> [String; 4] {
    [
        format!("--vips-concurrency={VIPS_THREADS}"),
        "--vips-cache-max=0".into(),
        format!("--vips-cache-max-memory={VIPS_CACHE_MAX_MEMORY}"),
        format!("--vips-disc-threshold={VIPS_DISC_THRESHOLD}"),
    ]
}

pub fn worker_main() -> Result<(), String> {
    std::env::set_var("VIPS_DISC_THRESHOLD", VIPS_DISC_THRESHOLD);
    let app = VipsApp::new("aperture-image-worker", false).map_err(|e| e.to_string())?;
    app.concurrency_set(VIPS_THREADS);
    app.cache_set_max(0);
    app.cache_set_max_mem(VIPS_CACHE_MAX_MEMORY);
    let mut worker = Worker {
        images: HashMap::new(),
        source: None,
    };
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        let mut line = Vec::new();
        let read = input
            .by_ref()
            .take((MAX_MESSAGE + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_MESSAGE || line.last() != Some(&b'\n') {
            return Err("Image worker request exceeds its size limit or is incomplete.".into());
        }
        let result = serde_json::from_slice::<Request>(&line)
            .map_err(|e| format!("Invalid image worker request: {e}"))
            .and_then(|request| worker.process(request));
        let result = result.map_err(|error| {
            let native = app.error_buffer().unwrap_or("").trim();
            let message = if native.is_empty() {
                error
            } else {
                format!("{error}: {native}")
            };
            app.error_clear();
            message.chars().take(4096).collect()
        });
        // Optional absent metadata fields also append errors to libvips' buffer.
        app.error_clear();
        write_response(&Response::Complete { result })?;
    }
    // Drop all image handles before VipsApp shuts libvips down.
    drop(worker);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_coarse_monotonic_throttled_and_finishes() {
        let progress = Progress::new();
        let now = Instant::now();
        assert_eq!(progress.accept(f64::NAN, now), None);
        assert_eq!(progress.accept(0.0, now), Some(0.0));
        assert_eq!(
            progress.accept(0.25, now + Duration::from_millis(100)),
            None
        );
        assert_eq!(
            progress.accept(0.25, now + Duration::from_millis(250)),
            Some(0.2)
        );
        assert_eq!(progress.accept(0.1, now + Duration::from_secs(1)), None);
        assert_eq!(
            progress.accept(1.0, now + Duration::from_millis(251)),
            Some(1.0)
        );
        assert_eq!(progress.accept(1.0, now + Duration::from_secs(2)), None);
        let progress = Progress::new();
        let updates = (0..=1000)
            .filter_map(|n| progress.accept(n as f64 / 1000.0, now + Duration::from_millis(n)))
            .collect::<Vec<_>>();
        assert_eq!(updates.first(), Some(&0.0));
        assert_eq!(updates.last(), Some(&1.0));
        assert!(updates.len() <= 11);
        assert_eq!(
            serde_json::to_string(&Response::Complete {
                result: Ok("used".into())
            })
            .unwrap(),
            r#"{"result":{"Ok":"used"}}"#
        );
    }

    #[test]
    fn validates_operation_numbers_and_pyramid_options() {
        assert!(positive("NaN").is_err());
        assert!(positive("-1").is_err());
        assert_eq!(positive("0.25").unwrap(), 0.25);
        assert!(numbers::<i32>("1 2 3", 4).is_err());
        assert!(dz_options(&["--suffix=.png[strip]".into(), "--overlap=0".into()]).is_ok());
        assert!(dz_options(&["--suffix=.jpg[Q=1]".into()]).is_err());
        assert_eq!(
            enum_name(
                "((VipsInterpretation) VIPS_INTERPRETATION_B_W)",
                "VIPS_INTERPRETATION_"
            ),
            "b-w"
        );
    }

    #[test]
    fn raw_and_channel_bridges_preserve_display_pixels() {
        // This is the only in-process VipsApp test. Other decoder tests use
        // isolated processes, so libvips initialization cannot race here.
        let app = VipsApp::new("aperture-raw-binding-test", false).unwrap();
        app.concurrency_set(2);
        app.cache_set_max(0);
        // Exercise the Windows I/O bridge against native decoding while keeping
        // the single in-process libvips initialization used by this test.
        for name in ["gray16-stripped.tif", "gray16-tiled-bigtiff.tif"] {
            let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures")
                .join(name);
            let native = VipsImage::new_from_file(source.to_str().unwrap()).unwrap();
            let bridged =
                VipsImage::new_from_file_handle(fs::File::open(source).unwrap(), "").unwrap();
            assert_eq!(bridged.get_width(), native.get_width());
            assert_eq!(bridged.get_height(), native.get_height());
            assert_eq!(
                bridged.get_as_string("format").unwrap(),
                native.get_as_string("format").unwrap()
            );
            assert_eq!(
                bridged.image_write_to_memory(),
                native.image_write_to_memory()
            );
        }
        let directory =
            std::env::temp_dir().join(format!("aperture-raw-binding-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/gray16-stripped.tif");
        let bmp = directory.join("source.bmp");
        let samples = vec![255, 0, 0, 0, 128, 255];
        image::RgbImage::from_raw(2, 1, samples.clone())
            .unwrap()
            .save_with_format(&bmp, image::ImageFormat::Bmp)
            .unwrap();
        {
            let decoded = crate::platform::open_image(bmp.to_str().unwrap()).unwrap();
            assert_eq!(decoded.image_write_to_memory(), samples);
            assert!(header(&decoded).unwrap().contains("vips-loader: bmp2vips"));
            let scratch = directory.join("native [page=3] 日本語.v");
            ops::vipssave(&decoded, scratch.to_str().unwrap()).unwrap();
            let reloaded = crate::platform::open_image(scratch.to_str().unwrap()).unwrap();
            assert_eq!(reloaded.image_write_to_memory(), samples);
            let copy = directory.join("bmp-copy.tif");
            let mut conversion = Worker {
                images: HashMap::new(),
                source: None,
            };
            conversion
                .command(
                    "vips",
                    &[
                        "tiffsave".into(),
                        bmp.to_string_lossy().into_owned(),
                        copy.to_string_lossy().into_owned(),
                        "--compression=lzw".into(),
                    ],
                )
                .unwrap();
            let saved = VipsImage::new_from_file(copy.to_str().unwrap()).unwrap();
            assert_eq!(saved.image_write_to_memory(), samples);
        }
        let actual = directory.join("actual.jpg");
        let expected = directory.join("expected.jpg");
        let mut worker = Worker {
            images: HashMap::new(),
            source: None,
        };
        let channel_directory = directory.join("channel-previews");
        worker
            .process(Request::ChannelPreviews {
                input: source.to_str().unwrap().into(),
                options: vec!["[page=0,n=1,access=sequential]".into()],
                scale: vec![1.0],
                planes: vec![Plane { page: 0, band: 0 }],
                ushort: true,
                directory: channel_directory.to_str().unwrap().into(),
            })
            .unwrap();
        // The 16-bit preview keeps every source sample at scale 1.
        let (width, height, preview) =
            u16_samples(&channel_directory.join("channels/0/preview.png"));
        let source_image = VipsImage::new_from_file(source.to_str().unwrap()).unwrap();
        assert_eq!(
            (width, height),
            (
                source_image.get_width() as u32,
                source_image.get_height() as u32
            )
        );
        assert_eq!(
            preview,
            source_image
                .image_write_to_memory()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|sample| u16::from_ne_bytes(*sample))
                .collect::<Vec<_>>()
        );
        drop(source_image);
        assert!(!channel_directory.join("preview.jpg").exists());
        assert_eq!(
            worker
                .process(Request::RawPreview {
                    input: source.to_str().unwrap().into(),
                    output: actual.to_str().unwrap().into(),
                    max_edge: 1024,
                    threads: 2,
                })
                .unwrap(),
            "used"
        );
        // Large 16-bit gray channel previews use the same area reducer losslessly.
        let raw_channel = directory.join("raw-channel.png");
        assert_eq!(
            worker
                .process(Request::RawPreview {
                    input: source.to_str().unwrap().into(),
                    output: raw_channel.to_str().unwrap().into(),
                    max_edge: 1024,
                    threads: 2,
                })
                .unwrap(),
            "used"
        );
        assert_eq!(u16_samples(&raw_channel), (width, height, preview));
        let status = std::process::Command::new("vips")
            .args([
                "resize",
                &format!("{}[page=0,n=1,access=sequential]", source.display()),
                &format!("{}[Q=85,strip,background=52 54 51]", expected.display()),
                "1",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let actual = worker.open(actual.to_str().unwrap()).unwrap();
        let expected = worker.open(expected.to_str().unwrap()).unwrap();
        assert_eq!(actual.get_width(), 65);
        assert_eq!(actual.get_height(), 49);
        let difference = ops::subtract(&actual, &expected)
            .and_then(|i| ops::abs(&i))
            .unwrap();
        assert_eq!(ops::max(&difference).unwrap(), 0.0);
        drop(difference);
        drop(expected);
        drop(actual);
        // A tiny sink can finish before libvips emits an intermediate eval.
        // Exercise the real GLib signal deterministically, without depending
        // on image size, thread scheduling, or another VipsApp initialization.
        let progress = Progress::new();
        unsafe {
            use libvips::bindings;
            let image = bindings::vips_image_new();
            assert!(!image.is_null());
            let mut native_progress: bindings::VipsProgress = std::mem::zeroed();
            native_progress.percent = 50;
            {
                let _hook = EvalProgress::new(&progress, 0.0, 1.0).unwrap();
                // Both the image and payload stay alive during synchronous emission.
                bindings::g_signal_emit_by_name(
                    image.cast(),
                    c"eval".as_ptr(),
                    &native_progress as *const bindings::VipsProgress,
                );
            }
            let reported = progress.last.lock().unwrap().take().unwrap().0;
            // Removal must stop callbacks before their context is freed.
            bindings::g_signal_emit_by_name(
                image.cast(),
                c"eval".as_ptr(),
                &native_progress as *const bindings::VipsProgress,
            );
            bindings::g_object_unref(image.cast());
            assert_eq!(reported, 5);
            assert!(progress.last.lock().unwrap().is_none());
        }
        let native_image = worker.open(source.to_str().unwrap()).unwrap();
        save_image(
            &native_image,
            directory.join("progress.jpg").to_str().unwrap(),
            &progress,
            0.0,
            1.0,
        )
        .unwrap();
        // Error returns remove their emission hook before callback data drops.
        assert!(save_image(
            &native_image,
            directory.join("missing/output.jpg").to_str().unwrap(),
            &progress,
            0.0,
            1.0
        )
        .is_err());
        drop(native_image);
        channel_pyramids_match_materialized_planes(&mut worker, &directory);
        constant_ushort_pyramid_is_exact_at_every_level(&mut worker, &directory);
        drop(worker);
        fs::remove_dir_all(directory).unwrap();
    }

    fn channel_pyramids_match_materialized_planes(worker: &mut Worker, directory: &Path) {
        for (bits, bands, band) in [(8, 3, 2), (16, 3, 1), (16, 1, 0)] {
            // Odd edges, more than one tile, and varying low bytes catch
            // accidental changes to normalization/shrink order at every level.
            let samples = (0..259 * 257 * bands)
                .flat_map(|i| {
                    let sample = ((i * 7919 + i / 259 * 257) % 65536) as u16;
                    if bits == 16 {
                        sample.to_ne_bytes().to_vec()
                    } else {
                        vec![(sample >> 8) as u8]
                    }
                })
                .collect::<Vec<_>>();
            let image = VipsImage::new_from_memory_copy(
                &samples,
                259,
                257,
                bands,
                if bits == 16 {
                    ops::BandFormat::Ushort
                } else {
                    ops::BandFormat::Uchar
                },
            )
            .unwrap();
            let source = directory.join(format!("channel-{bits}-{bands}.tif"));
            reinterpret(
                &image,
                match (bits, bands) {
                    (16, 1) => ops::Interpretation::Grey16,
                    (16, _) => ops::Interpretation::Rgb16,
                    _ => ops::Interpretation::Srgb,
                },
            )
            .unwrap()
            .image_write_to_file(source.to_str().unwrap())
            .unwrap();
            let source = format!("{}[page=0,n=1,access=sequential]", source.display());
            if bits == 8 {
                rgb_direct_pyramid_matches(worker, directory, &source);
            }
            let actual = directory.join(format!("actual-{bits}-{bands}"));
            worker
                .process(Request::ChannelPyramid {
                    input: source.clone(),
                    output: actual.to_str().unwrap().into(),
                    band,
                    ushort: bits == 16,
                })
                .unwrap();
            let raw = directory.join("raw.v").to_str().unwrap().to_owned();
            let extracted = directory.join("extracted.v").to_str().unwrap().to_owned();
            worker
                .command(
                    "vips",
                    &[
                        "extract_band".into(),
                        source,
                        raw.clone(),
                        band.to_string(),
                        "--n=1".into(),
                    ],
                )
                .unwrap();
            worker
                .command(
                    "vips",
                    &[
                        "copy".into(),
                        raw,
                        extracted.clone(),
                        if bits == 16 {
                            "--interpretation=grey16"
                        } else {
                            "--interpretation=b-w"
                        }
                        .into(),
                    ],
                )
                .unwrap();
            let expected = directory.join(format!("expected-{bits}-{bands}"));
            worker
                .command(
                    "vips",
                    &[
                        "dzsave".into(),
                        extracted,
                        expected.to_str().unwrap().into(),
                        "--tile-size=256".into(),
                        "--overlap=0".into(),
                        format!("--{}", crate::backend::pyramid::channel_suffix(bits == 16)),
                        "--depth=onepixel".into(),
                    ],
                )
                .unwrap();
            let actual_tiles = actual.with_file_name(format!("actual-{bits}-{bands}_files"));
            let expected_tiles = expected.with_file_name(format!("expected-{bits}-{bands}_files"));
            if bits == 16 {
                // Full resolution keeps exact source samples, including low bytes.
                let level =
                    actual_tiles.join(crate::backend::assets::max_level(259, 257).to_string());
                for (tile, x0, y0) in [("0_0", 0, 0), ("1_0", 256, 0), ("1_1", 256, 256)] {
                    let (width, height, values) = u16_samples(&level.join(format!("{tile}.png")));
                    assert_eq!((width, height), ((259 - x0).min(256), (257 - y0).min(256)));
                    for (index, value) in values.iter().enumerate() {
                        let (x, y) = (x0 + index as u32 % width, y0 + index as u32 / width);
                        let offset = ((y * 259 + x) * bands as u32 + band) as usize * 2;
                        assert_eq!(
                            *value,
                            u16::from_ne_bytes([samples[offset], samples[offset + 1]])
                        );
                    }
                }
            }
            for level in fs::read_dir(&actual_tiles).unwrap() {
                let level = level.unwrap();
                if !level.file_type().unwrap().is_dir() {
                    continue;
                }
                for tile in fs::read_dir(level.path()).unwrap() {
                    let tile = tile.unwrap();
                    let expected_tile = expected_tiles
                        .join(level.file_name())
                        .join(tile.file_name());
                    if bits == 16 {
                        // Every generated level stays a 16-bit grayscale PNG.
                        u16_samples(&tile.path());
                    }
                    let a = VipsImage::new_from_file(tile.path().to_str().unwrap()).unwrap();
                    let b = VipsImage::new_from_file(expected_tile.to_str().unwrap()).unwrap();
                    assert_eq!(
                        (a.get_width(), a.get_height()),
                        (b.get_width(), b.get_height())
                    );
                    assert_eq!(
                        a.image_write_to_memory(),
                        b.image_write_to_memory(),
                        "{}",
                        tile.path().display()
                    );
                }
            }
        }
    }

    fn u16_samples(path: &Path) -> (u32, u32, Vec<u16>) {
        let body = crate::backend::assets::u16_body(&fs::read(path).unwrap(), |_, _| true)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let dimension = |at: usize| u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        let samples = body[8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| u16::from_le_bytes(*sample))
            .collect();
        (dimension(0), dimension(4), samples)
    }

    fn constant_ushort_pyramid_is_exact_at_every_level(worker: &mut Worker, directory: &Path) {
        // A constant whose low byte differs from its high byte exposes any
        // 16-to-8-bit cast or rescaling at a reduced level.
        let samples = 0x1234_u16.to_ne_bytes().repeat(300 * 260);
        let image = VipsImage::new_from_memory_copy(&samples, 300, 260, 1, ops::BandFormat::Ushort)
            .unwrap();
        let source = directory.join("constant.tif");
        reinterpret(&image, ops::Interpretation::Grey16)
            .unwrap()
            .image_write_to_file(source.to_str().unwrap())
            .unwrap();
        let output = directory.join("constant");
        worker
            .process(Request::ChannelPyramid {
                input: format!("{}[page=0,n=1,access=sequential]", source.display()),
                output: output.to_str().unwrap().into(),
                band: 0,
                ushort: true,
            })
            .unwrap();
        let mut levels = 0;
        for level in fs::read_dir(directory.join("constant_files")).unwrap() {
            let level = level.unwrap();
            if !level.file_type().unwrap().is_dir() {
                continue;
            }
            levels += 1;
            for tile in fs::read_dir(level.path()).unwrap() {
                let (_, _, values) = u16_samples(&tile.unwrap().path());
                assert!(values.iter().all(|value| *value == 0x1234));
            }
        }
        assert_eq!(levels, crate::backend::assets::max_level(300, 260) + 1);
    }

    fn rgb_direct_pyramid_matches(worker: &mut Worker, directory: &Path, source: &str) {
        for (name, jpeg) in [
            ("rgb-direct", "--Q=85"),
            (
                "rgb-suffix",
                "--suffix=.jpg[Q=85,strip,background=52 54 51]",
            ),
        ] {
            worker
                .command(
                    "vips",
                    &[
                        "dzsave".into(),
                        source.into(),
                        directory.join(name).to_str().unwrap().into(),
                        "--tile-size=256".into(),
                        "--overlap=0".into(),
                        jpeg.into(),
                        "--background=52 54 51".into(),
                        "--depth=onepixel".into(),
                    ],
                )
                .unwrap();
            assert_eq!(unsafe { libvips::bindings::vips_concurrency_get() }, 2);
        }
        for level in fs::read_dir(directory.join("rgb-direct_files")).unwrap() {
            let level = level.unwrap();
            if !level.file_type().unwrap().is_dir() {
                continue;
            }
            for tile in fs::read_dir(level.path()).unwrap() {
                let tile = tile.unwrap();
                let expected = directory
                    .join("rgb-suffix_files")
                    .join(level.file_name())
                    .join(tile.file_name())
                    .with_extension("jpg");
                let a = VipsImage::new_from_file(tile.path().to_str().unwrap()).unwrap();
                let b = VipsImage::new_from_file(expected.to_str().unwrap()).unwrap();
                assert_eq!(a.image_write_to_memory(), b.image_write_to_memory());
            }
        }
    }
}
