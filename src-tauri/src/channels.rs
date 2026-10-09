//! Explicit TIFF channel layouts. Ordinary pages never imply channels.
use crate::backend::field;
use serde::{Deserialize, Serialize};

pub const MAX_CHANNELS: u32 = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub color: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plane {
    pub page: u32,
    pub band: u32,
}

pub struct Layout {
    pub channels: Vec<Channel>,
    pub planes: Vec<Plane>,
    pub channel_mode: bool,
}

fn number(header: &str, key: &str) -> Result<u32, String> {
    field(header, key)
        .unwrap_or("1")
        .parse()
        .map_err(|_| format!("Invalid TIFF {key}."))
}

pub fn discover(
    header: &str,
    inspect: &dyn Fn(&str) -> Result<String, String>,
    tiffinfo: Option<&str>,
) -> Result<Layout, String> {
    let bands = number(header, "bands")?;
    let tiff = field(header, "vips-loader") == Some("tiffload");
    let info = tiffinfo
        .unwrap_or("")
        .split("=== TIFF directory 1")
        .next()
        .unwrap_or("");
    let alpha = info
        .lines()
        .any(|line| line.contains("Extra Samples:") && line.contains("-alpha"));
    let gray = matches!(
        field(header, "interpretation"),
        Some("b-w" | "grey16" | "multiband")
    );
    if tiff
        && bands > 1
        && ((alpha && (gray || bands != 4)) || (gray && bands == 2 && tiffinfo.is_none()))
    {
        return Err("TIFF grayscale/multiband alpha layouts are not supported. Alpha cannot be treated as an image channel.".into());
    }
    let description = field(header, "image-description").unwrap_or("");
    // OME needs scene and TiffData/UUID mapping, not a guess from page order.
    if tiff && header.contains("<OME") {
        return Err("OME-TIFF channel/dimension mapping is not supported. Export an ImageJ hyperstack or an interleaved multiband TIFF.".into());
    }
    let mut planes = Vec::new();
    let imagej = tiff && description.starts_with("ImageJ=");
    let channel_mode = if imagej {
        let metadata = header
            .split_once("image-description: ")
            .unwrap()
            .1
            .split("\n\n")
            .next()
            .unwrap();
        let dimension = |key: &str| -> Result<u32, String> {
            metadata
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}=")))
                .unwrap_or("1")
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("Invalid ImageJ {key} dimension."))
        };
        let count = dimension("channels")?;
        let images = dimension("images")?;
        let product = count
            .checked_mul(dimension("slices")?)
            .and_then(|value| value.checked_mul(dimension("frames").ok()?));
        if (count > 1
            || metadata
                .lines()
                .any(|line| line.starts_with("slices=") || line.starts_with("frames=")))
            && (product != Some(images) || number(header, "n-pages")? != images)
        {
            return Err("ImageJ dimensions do not match readable TIFF pages. Contiguous single-directory hyperstacks are not supported.".into());
        }
        if count > MAX_CHANNELS {
            return Err(format!(
                "At most {MAX_CHANNELS} image channels are supported."
            ));
        }
        if count > 1 {
            if bands != 1 {
                return Err("ImageJ channel pages must contain one grayscale band each.".into());
            }
            // ImageJ orders C fastest, then Z, then T. Only first Z/T is shown.
            for page in 0..count {
                if page > 0 {
                    let candidate = inspect(&format!("[page={page},n=1]"))?;
                    if [
                        "width",
                        "height",
                        "bands",
                        "format",
                        "orientation",
                        "interpretation",
                    ]
                    .iter()
                    .any(|key| field(header, key) != field(&candidate, key))
                    {
                        return Err(
                            "ImageJ channel pages have inconsistent dimensions or pixels.".into(),
                        );
                    }
                }
                planes.push(Plane { page, band: 0 });
            }
        }
        count > 1
    } else {
        tiff && ((gray && bands > 1)
            || !matches!(bands, 1 | 3 | 4)
            || (bands == 4 && !alpha && tiffinfo.is_some()))
    };
    if channel_mode && bands > 1 && info.contains("Planar Configuration: separate image planes") {
        return Err(
            "Planar-separate multiband TIFFs are not supported. Export interleaved bands.".into(),
        );
    }
    if planes.is_empty() {
        let count = if channel_mode { bands } else { bands.min(3) };
        if count == 0 || count > MAX_CHANNELS {
            return Err(format!(
                "At most {MAX_CHANNELS} image channels are supported."
            ));
        }
        planes = (0..count).map(|band| Plane { page: 0, band }).collect();
    }
    let rgb = !channel_mode && planes.len() == 3;
    let colors = [
        "#ff0000", "#00ff00", "#0000ff", "#ff00ff", "#00ffff", "#ffff00", "#ffffff",
    ];
    let names = ["Red", "Green", "Blue"];
    let channels = planes
        .iter()
        .enumerate()
        .map(|(index, _)| Channel {
            name: if rgb {
                names[index].into()
            } else {
                format!("Channel {}", index + 1)
            },
            color: if planes.len() == 1 {
                "#ffffff"
            } else {
                colors[index % colors.len()]
            }
            .into(),
        })
        .collect();
    Ok(Layout {
        channels,
        planes,
        channel_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header(bands: u32) -> String {
        format!("width: 12\nheight: 9\nbands: {bands}\nformat: ushort\ninterpretation: grey16\nvips-loader: tiffload\norientation: 1\nn-pages: 12\n")
    }
    #[test]
    fn channel_pages_require_explicit_consistent_imagej_dimensions() {
        let ordinary = header(1);
        assert!(
            !discover(&ordinary, &|_| unreachable!(), None)
                .unwrap()
                .channel_mode
        );
        let simple_stack = format!("{}image-description: ImageJ=1.54\nimages=12\n\n", ordinary);
        assert!(
            !discover(&simple_stack, &|_| unreachable!(), None)
                .unwrap()
                .channel_mode
        );
        let imagej = format!(
            "{}image-description: ImageJ=1.54\nimages=12\nchannels=3\nslices=2\nframes=2\n\n",
            ordinary
        );
        let layout = discover(&imagej, &|_| Ok(ordinary.clone()), None).unwrap();
        assert!(layout.channel_mode);
        assert_eq!(
            layout
                .planes
                .iter()
                .map(|plane| plane.page)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(discover(
            &imagej.replace("images=12", "images=11"),
            &|_| Ok(ordinary.clone()),
            None
        )
        .is_err());
        assert!(discover(
            &imagej,
            &|_| Ok(ordinary.replace("width: 12", "width: 13")),
            None
        )
        .is_err());
        assert!(discover(
            &imagej.replace("channels=3", "channels=33"),
            &|_| unreachable!(),
            None
        )
        .is_err());
        assert!(discover(
            &imagej.replace("n-pages: 12", "n-pages: 1"),
            &|_| unreachable!(),
            None
        )
        .is_err());
    }
    #[test]
    fn alpha_is_not_an_image_channel() {
        let gray_alpha = header(2);
        assert!(discover(
            &gray_alpha,
            &|_| unreachable!(),
            Some("Extra Samples: 1<unassoc-alpha>")
        )
        .is_err());
        assert!(discover(&gray_alpha, &|_| unreachable!(), None).is_err());
        assert!(
            discover(
                &gray_alpha,
                &|_| unreachable!(),
                Some("Extra Samples: 1<unspecified>")
            )
            .unwrap()
            .channel_mode
        );
        let rgba = header(4).replace("grey16", "rgb16");
        assert!(
            !discover(
                &rgba,
                &|_| unreachable!(),
                Some("Extra Samples: 1<unassoc-alpha>")
            )
            .unwrap()
            .channel_mode
        );
        assert!(
            discover(
                &header(4),
                &|_| unreachable!(),
                Some("Extra Samples: 3<unspecified, unspecified, unspecified>")
            )
            .unwrap()
            .channel_mode
        );
    }
    #[test]
    fn multiband_and_rgb_are_distinct_and_ome_is_explicitly_rejected() {
        assert!(
            discover(&header(5), &|_| unreachable!(), None)
                .unwrap()
                .channel_mode
        );
        let rgb = discover(
            &header(3).replace("grey16", "rgb16"),
            &|_| unreachable!(),
            None,
        )
        .unwrap();
        assert!(!rgb.channel_mode);
        assert_eq!(rgb.channels[0].name, "Red");
        assert_eq!(rgb.channels[2].color, "#0000ff");
        assert!(discover(&header(33), &|_| unreachable!(), None).is_err());
        assert!(discover(
            &format!("{}image-description: <OME SizeC=\"5\">", header(1)),
            &|_| unreachable!(),
            None
        )
        .is_err());
    }
}
