//! Explicit TIFF channel layouts. Ordinary pages never imply channels.
use crate::backend::pyramid::field;
use serde::{Deserialize, Serialize};

pub const MAX_CHANNELS: u32 = 32;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub color: String,
    /// Display sample range in source units.
    #[serde(default = "eight_bit")]
    pub domain: [u16; 2],
}

fn eight_bit() -> [u16; 2] {
    domain(8)
}

pub fn domain(sample_bits: u8) -> [u16; 2] {
    [0, if sample_bits == 16 { u16::MAX } else { 255 }]
}

/// One display channel: a band of a TIFF page in a source file. Member 0 is
/// the opened image; OME-TIFF members are numbered as in their dataset.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plane {
    #[serde(default)]
    pub member: u32,
    pub page: u32,
    pub band: u32,
}

pub struct Layout {
    pub channels: Vec<Channel>,
    pub planes: Vec<Plane>,
    pub channel_mode: bool,
    pub rgb: bool,
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
    // 16-bit gray and RGB keep lossless samples as channels; 16-bit RGBA stays RGBA.
    let wide = field(header, "format") == Some("ushort") && matches!(bands, 1 | 3);
    if tiff
        && bands > 1
        && ((alpha && (gray || bands != 4)) || (gray && bands == 2 && tiffinfo.is_none()))
    {
        return Err("TIFF grayscale/multiband alpha layouts are not supported. Alpha cannot be treated as an image channel.".into());
    }
    let description = field(header, "image-description").unwrap_or("");
    // OME-TIFFs are detected before this and use their TiffData mapping. Other
    // OME-XML, such as one behind a DOCTYPE, must not be guessed from page order.
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
                planes.push(Plane {
                    member: 0,
                    page,
                    band: 0,
                });
            }
        }
        count > 1 || wide
    } else {
        wide || (tiff
            && ((gray && bands > 1)
                || !matches!(bands, 1 | 3 | 4)
                || (bands == 4 && !alpha && tiffinfo.is_some())))
    };
    let rgb = bands == 3 && !gray;
    // libvips already assembles planar RGB for the ordinary RGB path.
    if channel_mode
        && bands > 1
        && !rgb
        && info.contains("Planar Configuration: separate image planes")
    {
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
        planes = (0..count)
            .map(|band| Plane {
                member: 0,
                page: 0,
                band,
            })
            .collect();
    }
    let sample_bits = if channel_mode && field(header, "format") == Some("ushort") {
        16
    } else {
        8
    };
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
            domain: domain(sample_bits),
        })
        .collect();
    Ok(Layout {
        channels,
        planes,
        channel_mode,
        rgb,
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
        let gray8 = ordinary.replace("ushort", "uchar").replace("grey16", "b-w");
        let simple_stack = "image-description: ImageJ=1.54\nimages=12\n\n";
        for header in [gray8.clone(), format!("{gray8}{simple_stack}")] {
            let layout = discover(&header, &|_| unreachable!(), None).unwrap();
            assert!(!layout.channel_mode);
            assert_eq!(layout.channels[0].domain, [0, 255]);
        }
        // Ordinary 16-bit gray pages and stacks show their first page losslessly.
        for header in [ordinary.clone(), format!("{ordinary}{simple_stack}")] {
            let layout = discover(&header, &|_| unreachable!(), None).unwrap();
            assert!(layout.channel_mode && !layout.rgb);
            assert_eq!(layout.planes.len(), 1);
            assert_eq!(layout.channels[0].name, "Channel 1");
            assert_eq!(layout.channels[0].color, "#ffffff");
            assert_eq!(layout.channels[0].domain, [0, 65535]);
        }
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
        let rgb16 = header(3).replace("grey16", "rgb16");
        let rgb8 = rgb16.replace("ushort", "uchar").replace("rgb16", "srgb");
        let planar = Some("Planar Configuration: separate image planes");
        for (header, channel_mode, maximum) in [(&rgb8, false, 255), (&rgb16, true, 65535)] {
            let rgb = discover(header, &|_| unreachable!(), planar).unwrap();
            assert_eq!(rgb.channel_mode, channel_mode);
            assert!(rgb.rgb);
            assert_eq!(rgb.channels[0].name, "Red");
            assert_eq!(rgb.channels[2].color, "#0000ff");
            assert_eq!(rgb.channels[1].domain, [0, maximum]);
        }
        assert!(discover(&header(3), &|_| unreachable!(), planar).is_err());
        assert!(discover(&header(33), &|_| unreachable!(), None).is_err());
        assert!(discover(
            &format!("{}image-description: <OME SizeC=\"5\">", header(1)),
            &|_| unreachable!(),
            None
        )
        .is_err());
    }
}
