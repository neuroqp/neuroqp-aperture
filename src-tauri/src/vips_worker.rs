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
};

pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
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
    RawPreview {
        input: String,
        output: String,
        max_edge: u32,
        threads: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub result: Result<String, String>,
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
        let image = Rc::new(VipsImage::new_from_file(input).map_err(|e| e.to_string())?);
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
        match request {
            Request::Command { program, args } => self.command(&program, &args),
            Request::ChannelPreviews {
                input,
                options,
                scale,
                planes,
                ushort,
                directory,
            } => {
                self.channel_previews(&input, &options, &scale, &planes, ushort, &directory)?;
                Ok(String::new())
            }
            Request::RawPreview {
                input,
                output,
                max_edge,
                threads,
            } => {
                if max_edge == 0 || max_edge > 1024 || threads == 0 || threads > 8 {
                    return Err("Invalid raw TIFF preview limits.".into());
                }
                let Some(preview) =
                    crate::raw_tiff::preview_with_threads(Path::new(&input), max_edge, threads)
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
                reinterpret(&image, interpretation)?
                    .image_write_to_file(&format!("{output}[Q=85,strip,background=52 54 51]"))
                    .map_err(|e| e.to_string())?;
                Ok("used".into())
            }
        }
    }

    fn command(&mut self, program: &str, args: &[String]) -> Result<String, String> {
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
            let values = pixel
                .image_write_to_memory()
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
                resized
                    .image_write_to_file(output)
                    .map_err(|e| e.to_string())
            }
            "extract_band" if args.len() == 5 && args[4] == "--n=1" => {
                let band = args[3].parse::<i32>().map_err(|_| "Invalid image band.")?;
                ops::extract_band(&image, band)
                    .and_then(|i| i.image_write_to_file(output))
                    .map_err(|e| e.to_string())
            }
            "cast" if args.len() == 5 && args[3] == "uchar" && args[4] == "--shift" => {
                ops::cast_with_opts(
                    &image,
                    ops::BandFormat::Uchar,
                    &ops::CastOptions { shift: true },
                )
                .and_then(|i| i.image_write_to_file(output))
                .map_err(|e| e.to_string())
            }
            "copy" if args.len() == 3 => {
                image.image_write_to_file(output).map_err(|e| e.to_string())
            }
            "copy" if args.len() == 4 && args[3] == "--interpretation=b-w" => grayscale(&image)?
                .image_write_to_file(output)
                .map_err(|e| e.to_string()),
            "dzsave" => {
                let options = dz_options(&args[3..])?;
                image
                    .image_write_to_file(&format!("{output}.dz[container=fs,{options}]"))
                    .map_err(|e| e.to_string())
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
                ops::affine_with_opts(
                    &image, matrix[0], matrix[1], matrix[2], matrix[3], &settings,
                )
                .and_then(|i| i.image_write_to_file(output))
                .map_err(|e| e.to_string())
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
        directory: &str,
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
                let memory = VipsImage::image_copy_memory(small).map_err(|e| e.to_string())?;
                resized.insert(key.clone(), memory);
                self.images.remove(&source_name);
            }
            let band = i32::try_from(plane.band).map_err(|_| "Invalid channel band.")?;
            let extracted = ops::extract_band(&resized[&key], band).map_err(|e| e.to_string())?;
            let pixels = if ushort {
                ops::cast_with_opts(
                    &extracted,
                    ops::BandFormat::Uchar,
                    &ops::CastOptions { shift: true },
                )
                .map_err(|e| e.to_string())?
            } else {
                extracted
            };
            let display = grayscale(&pixels)?;
            let channel = Path::new(directory)
                .join("channels")
                .join(index.to_string());
            fs::create_dir_all(&channel).map_err(|e| e.to_string())?;
            let partial = channel.join("preview.partial.png");
            display
                .image_write_to_file(partial.to_str().ok_or("Invalid preview path.")?)
                .map_err(|e| e.to_string())?;
            fs::rename(partial, channel.join("preview.png")).map_err(|e| e.to_string())?;
            if index == 0 {
                display
                    .image_write_to_file(
                        Path::new(directory)
                            .join("preview.jpg")
                            .to_str()
                            .ok_or("Invalid preview path.")?,
                    )
                    .map_err(|e| e.to_string())?;
            }
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

fn grayscale(image: &VipsImage) -> Result<VipsImage, String> {
    reinterpret(image, ops::Interpretation::BW)
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
                | "suffix=.jpg[Q=85,strip,background=52 54 51]"
        ) {
            return Err("Unsupported pyramid option.".into());
        }
        options.push(option);
    }
    Ok(options.join(","))
}

pub fn worker_main() -> Result<(), String> {
    std::env::set_var("VIPS_DISC_THRESHOLD", "32m");
    let app = VipsApp::new("aperture-image-worker", false).map_err(|e| e.to_string())?;
    app.concurrency_set(2);
    app.cache_set_max(0);
    app.cache_set_max_mem(64 * 1024 * 1024);
    let mut worker = Worker {
        images: HashMap::new(),
        source: None,
    };
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::stdout().lock();
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
        let response = serde_json::to_vec(&Response { result }).map_err(|e| e.to_string())?;
        if response.len() >= MAX_MESSAGE {
            return Err("Image worker response exceeds its size limit.".into());
        }
        stdout
            .write_all(&response)
            .and_then(|_| stdout.write_all(b"\n"))
            .and_then(|_| stdout.flush())
            .map_err(|e| e.to_string())?;
    }
    // Drop all image handles before VipsApp shuts libvips down.
    drop(worker);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn raw_ushort_bridge_preserves_high_byte_display_pixels() {
        // This is the only in-process VipsApp test. Other decoder tests use
        // isolated processes, so libvips initialization cannot race here.
        let app = VipsApp::new("aperture-raw-binding-test", false).unwrap();
        app.concurrency_set(2);
        app.cache_set_max(0);
        let directory =
            std::env::temp_dir().join(format!("aperture-raw-binding-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/gray16-stripped.tif");
        let actual = directory.join("actual.jpg");
        let expected = directory.join("expected.jpg");
        let mut worker = Worker {
            images: HashMap::new(),
            source: None,
        };
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
        drop(worker);
        fs::remove_dir_all(directory).unwrap();
    }
}
