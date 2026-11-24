use std::{
    fs::File,
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Result, ensure};
use image::{ImageFormat, ImageReader, Limits};
use url::Url;

pub const MAX_SIZE: u64 = 8 * 1024 * 1024;

pub struct Icon {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

impl Icon {
    pub fn open(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_SIZE + 1)
            .read_to_end(&mut bytes)?;
        Self::from_bytes(bytes, Some(path))
    }

    pub fn from_bytes(bytes: Vec<u8>, path: Option<&Path>) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() as u64 <= MAX_SIZE,
            "Icon must be at most 8 MiB"
        );
        if path
            .and_then(Path::extension)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
            || std::str::from_utf8(&bytes).is_ok_and(|document| document.contains("<svg"))
        {
            let document = std::str::from_utf8(&bytes)?;
            ensure!(document.contains("<svg"), "Invalid SVG icon");
            return Ok(Self {
                bytes,
                extension: "svg",
            });
        }

        let format = image::guess_format(&bytes)?;
        let extension = match format {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::WebP => "webp",
            ImageFormat::Ico => "ico",
            _ => anyhow::bail!("Choose a PNG, SVG, JPEG, WebP, or ICO icon"),
        };
        let mut reader = ImageReader::with_format(std::io::Cursor::new(&bytes), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        reader.limits(limits);
        let image = reader.decode()?;
        if extension == "png" {
            return Ok(Self { bytes, extension });
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png)?;
        Ok(Self {
            bytes: png.into_inner(),
            extension: "png",
        })
    }
}

pub fn favicon(url: &Url) -> Option<Icon> {
    let mut service = Url::parse("https://www.google.com/s2/favicons").ok()?;
    service
        .query_pairs_mut()
        .append_pair("domain", url.host_str()?)
        .append_pair("sz", "128");
    let output = Command::new("curl")
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--location",
            "--max-time",
            "5",
            "--max-filesize",
            "1048576",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            service.as_str(),
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| Icon::from_bytes(output.stdout, None).ok())
        .flatten()
}
