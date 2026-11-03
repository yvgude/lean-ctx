// SPDX-License-Identifier: Apache-2.0
use std::io::Read;

use base64::Engine;
use rmcp::ErrorData;
use rmcp::model::ContentBlock;

use crate::server::tool_trait::ToolOutput;

fn open_image_nofollow(path: &str) -> Result<std::fs::File, ErrorData> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|error| ErrorData::invalid_params(format!("Cannot read image: {error}"), None))?;
    let metadata = file
        .metadata()
        .map_err(|error| ErrorData::invalid_params(format!("Cannot read image: {error}"), None))?;
    if lean_ctx::core::pathutil::is_symlink_or_reparse(&metadata) || !metadata.is_file() {
        return Err(ErrorData::invalid_params(
            "Cannot read image: path is not a regular non-symlink file",
            None,
        ));
    }
    Ok(file)
}

/// Read one bounded image descriptor into MCP multimodal content blocks.
pub(super) fn read_image_file(path: &str) -> Result<ToolOutput, ErrorData> {
    use lean_ctx::core::binary_detect::{IMAGE_MAX_BYTES, image_mime_type};

    let mut file = open_image_nofollow(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| ErrorData::invalid_params(format!("Cannot read image: {error}"), None))?;
    if metadata.len() > IMAGE_MAX_BYTES {
        return Err(ErrorData::invalid_params(
            format!(
                "Image too large ({:.1} MB, limit {:.0} MB). Resize or use a smaller image.",
                metadata.len() as f64 / 1024.0 / 1024.0,
                IMAGE_MAX_BYTES as f64 / 1024.0 / 1024.0,
            ),
            None,
        ));
    }
    let mime_type = image_mime_type(path)
        .ok_or_else(|| ErrorData::invalid_params("Unsupported image format".to_string(), None))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(IMAGE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ErrorData::invalid_params(format!("Cannot read image: {error}"), None))?;
    if bytes.len() as u64 > IMAGE_MAX_BYTES {
        return Err(ErrorData::invalid_params(
            "Image grew beyond the 20 MB limit while being read",
            None,
        ));
    }
    let base64_data = base64::prelude::BASE64_STANDARD.encode(&bytes);
    let short_name = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    Ok(ToolOutput::image(
        vec![
            ContentBlock::text(format!(
                "[Image: {} ({} KB, {})]",
                short_name,
                bytes.len() / 1024,
                mime_type
            )),
            ContentBlock::image(base64_data, mime_type),
        ],
        path.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::read_image_file;

    #[test]
    fn regular_image_is_read_from_one_bounded_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("fixture.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();

        let output = read_image_file(&image.to_string_lossy()).unwrap();

        assert_eq!(output.content_blocks.as_ref().map(Vec::len), Some(2));
        assert_eq!(output.path.as_deref(), image.to_str());
    }

    #[cfg(unix)]
    #[test]
    fn image_reader_refuses_a_symlink_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.png");
        let link = dir.path().join("link.png");
        std::fs::write(&target, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        symlink(target, &link).unwrap();

        assert!(read_image_file(&link.to_string_lossy()).is_err());
    }
}
