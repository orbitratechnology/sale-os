//! The business logo (SET-001, E04-T02): whatever the owner chooses (PNG or JPEG) becomes a
//! PNG of at most [`MAX_SIDE`] pixels a side, so receipts and backups stay small and the PDF
//! writer only ever sees one well-formed format.

use std::io::Cursor;

/// The longest side of a stored logo, in pixels.
pub const MAX_SIDE: u32 = 512;
/// A chosen file larger than this is refused before decoding.
pub const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
/// Images with more pixels than this are refused (a small file can claim a huge image).
const MAX_INPUT_PIXELS: u64 = 40_000_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LogoError {
    #[error("the file is larger than 10 MB")]
    TooLarge,
    #[error("the file is not a PNG or JPEG image")]
    NotAnImage,
    #[error("the image could not be read: {0}")]
    Unreadable(String),
}

/// A stored logo: PNG bytes and their size in pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logo {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Logo {
    /// Reads a stored logo (written by [`prepare`]).
    pub fn from_png(png: Vec<u8>) -> Result<Self, LogoError> {
        let img = decode_png(&png)?;
        Ok(Self {
            png,
            width: img.width,
            height: img.height,
        })
    }
}

/// RGBA, 8 bits per channel.
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Decodes a PNG or JPEG, shrinks it to fit [`MAX_SIDE`] (never enlarges), and encodes PNG.
pub fn prepare(input: &[u8]) -> Result<Logo, LogoError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(LogoError::TooLarge);
    }
    let img = if input.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(input)?
    } else if input.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(input)?
    } else {
        return Err(LogoError::NotAnImage);
    };
    let img = shrink(&img, MAX_SIDE);
    let png = encode_png(&img)?;
    Ok(Logo {
        png,
        width: img.width,
        height: img.height,
    })
}

fn unreadable(e: impl std::fmt::Display) -> LogoError {
    LogoError::Unreadable(e.to_string())
}

fn check_size(width: u32, height: u32) -> Result<(), LogoError> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_INPUT_PIXELS {
        return Err(LogoError::Unreadable(format!("{width}×{height} pixels")));
    }
    Ok(())
}

fn decode_png(input: &[u8]) -> Result<Rgba, LogoError> {
    let mut decoder = png::Decoder::new(Cursor::new(input));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(unreadable)?;
    let (w, h) = (reader.info().width, reader.info().height);
    check_size(w, h)?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or_else(|| unreadable("too large"))?
    ];
    let frame = reader.next_frame(&mut buf).map_err(unreadable)?;
    let data = &buf[..frame.buffer_size()];
    let pixels = match frame.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err(unreadable("palette not expanded")),
    };
    Ok(Rgba {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

fn decode_jpeg(input: &[u8]) -> Result<Rgba, LogoError> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(input), options);
    decoder.decode_headers().map_err(unreadable)?;
    let (w, h) = decoder.dimensions().ok_or_else(|| unreadable("no size"))?;
    let (w, h) = (
        u32::try_from(w).map_err(unreadable)?,
        u32::try_from(h).map_err(unreadable)?,
    );
    check_size(w, h)?;
    let pixels = decoder.decode().map_err(unreadable)?;
    if pixels.len() != w as usize * h as usize * 4 {
        return Err(unreadable("unexpected pixel data"));
    }
    Ok(Rgba {
        width: w,
        height: h,
        pixels,
    })
}

/// Box filter: each output pixel is the average of the input pixels it covers. Colour is
/// weighted by alpha, so transparent edges do not darken.
fn shrink(img: &Rgba, max: u32) -> Rgba {
    let longest = img.width.max(img.height);
    if longest <= max {
        return Rgba {
            width: img.width,
            height: img.height,
            pixels: img.pixels.clone(),
        };
    }
    // Rounded integer scaling: width × max / longest.
    let scaled = |side: u32| {
        ((u64::from(side) * u64::from(max) + u64::from(longest) / 2) / u64::from(longest)).max(1) as u32
    };
    let (ow, oh) = (scaled(img.width), scaled(img.height));
    let (iw, ih) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(ow as usize * oh as usize * 4);
    for oy in 0..oh as usize {
        let y0 = oy * ih / oh as usize;
        let y1 = ((oy + 1) * ih / oh as usize).max(y0 + 1);
        for ox in 0..ow as usize {
            let x0 = ox * iw / ow as usize;
            let x1 = ((ox + 1) * iw / ow as usize).max(x0 + 1);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * iw + x) * 4;
                    let alpha = u64::from(img.pixels[i + 3]);
                    r += u64::from(img.pixels[i]) * alpha;
                    g += u64::from(img.pixels[i + 1]) * alpha;
                    b += u64::from(img.pixels[i + 2]) * alpha;
                    a += alpha;
                    n += 1;
                }
            }
            let px = |c: u64| (c + a / 2).checked_div(a).unwrap_or(0) as u8;
            out.extend_from_slice(&[px(r), px(g), px(b), ((a + n / 2) / n) as u8]);
        }
    }
    Rgba {
        width: ow,
        height: oh,
        pixels: out,
    }
}

fn encode_png(img: &Rgba) -> Result<Vec<u8>, LogoError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(unreadable)?;
        writer.write_image_data(&img.pixels).map_err(unreadable)?;
        writer.finish().map_err(unreadable)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        encode_png(&Rgba {
            width: w,
            height: h,
            pixels: rgba.repeat(w as usize * h as usize),
        })
        .unwrap()
    }

    #[test]
    fn a_large_image_shrinks_to_512_keeping_its_shape() {
        let logo = prepare(&solid_png(1600, 400, [200, 10, 10, 255])).unwrap();
        assert_eq!((logo.width, logo.height), (512, 128));
        let back = Logo::from_png(logo.png.clone()).unwrap();
        assert_eq!((back.width, back.height), (512, 128));
        let img = decode_png(&logo.png).unwrap();
        assert_eq!(&img.pixels[..4], &[200, 10, 10, 255], "colour kept");
    }

    #[test]
    fn a_small_image_is_not_enlarged_and_transparency_is_kept() {
        let logo = prepare(&solid_png(64, 32, [0, 0, 255, 0])).unwrap();
        assert_eq!((logo.width, logo.height), (64, 32));
        assert_eq!(decode_png(&logo.png).unwrap().pixels[3], 0);
    }

    #[test]
    fn a_jpeg_becomes_a_png_of_at_most_512() {
        let jpeg = include_bytes!("../tests/fixtures/logo-800x600.jpg");
        let logo = prepare(jpeg).unwrap();
        assert_eq!((logo.width, logo.height), (512, 384));
        assert!(logo.png.starts_with(b"\x89PNG"));
        let img = decode_png(&logo.png).unwrap();
        // Top-left quarter white, the rest blue (JPEG is lossy: close, not exact).
        let at = |x: usize, y: usize| &img.pixels[(y * 512 + x) * 4..(y * 512 + x) * 4 + 4];
        assert!(at(10, 10)[0] > 240 && at(10, 10)[3] == 255);
        let blue = at(500, 370);
        assert!(blue[2] > 200 && blue[0] < 40, "{blue:?}");
    }

    #[test]
    fn other_files_are_refused() {
        assert_eq!(prepare(b"GIF89a....").unwrap_err(), LogoError::NotAnImage);
        assert_eq!(prepare(b"").unwrap_err(), LogoError::NotAnImage);
        assert!(matches!(
            prepare(b"\x89PNG\r\n\x1a\nbroken"),
            Err(LogoError::Unreadable(_))
        ));
        assert!(matches!(
            prepare(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Err(LogoError::Unreadable(_))
        ));
        assert_eq!(
            prepare(&vec![0; MAX_INPUT_BYTES + 1]).unwrap_err(),
            LogoError::TooLarge
        );
    }
}
