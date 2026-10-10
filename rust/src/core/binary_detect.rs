use std::path::Path;

const BINARY_EXTENSIONS: &[&str] = &[
    // Data formats
    "parquet",
    "avro",
    "orc",
    "arrow",
    "feather",
    "hdf5",
    "h5",
    "npy",
    "npz",
    // Databases
    "db",
    "sqlite",
    "sqlite3",
    "mdb",
    "accdb",
    "ldb",
    // Archives
    "zip",
    "gz",
    "tar",
    "bz2",
    "xz",
    "7z",
    "rar",
    "zst",
    "lz4",
    "lzma",
    // Images
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "bmp",
    "ico",
    "tiff",
    "tif",
    // No "svg": it is XML text that agents read and edit like source. A real
    // binary behind any extension is still caught by the content check.
    "psd",
    "raw",
    "cr2",
    "nef",
    "heic",
    "heif",
    "avif",
    // Audio/Video
    "mp3",
    "mp4",
    "wav",
    "flac",
    "ogg",
    "avi",
    "mkv",
    "mov",
    "webm",
    "m4a",
    // Executables/Libraries
    "exe",
    "dll",
    "so",
    "dylib",
    "o",
    "a",
    "obj",
    "lib",
    "pdb",
    "class",
    "jar",
    "war",
    "ear",
    // Compiled/Bytecode
    "pyc",
    "pyo",
    "whl",
    "egg",
    "beam",
    "wasm",
    "wast",
    // ML models
    "model",
    "onnx",
    "pt",
    "pth",
    "safetensors",
    "gguf",
    "ggml",
    "tflite",
    "pb",
    "h5",
    "keras",
    // Serialized
    "pkl",
    "pickle",
    "bin",
    "dat",
    "protobuf",
    // Documents (binary)
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "odt",
    "ods",
    // Fonts
    "ttf",
    "otf",
    "woff",
    "woff2",
    "eot",
    // Disk images
    "iso",
    "img",
    "vmdk",
    "qcow2",
];

/// Image formats that LLMs can process visually via multimodal input.
/// Only formats supported by all major providers (Anthropic, OpenAI, Google).
const LLM_VIEWABLE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Maximum file size for image passthrough (20 MB).
pub(crate) const IMAGE_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Fast extension-based binary detection (zero I/O).
pub(crate) fn has_binary_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| BINARY_EXTENSIONS.contains(&ext.as_str()))
}

/// Heuristic: read first 8 KB and check for NULL bytes.
/// Standard method used by `file(1)`, git, etc.
fn has_binary_content(path: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; 8192];
    let mut reader = std::io::BufReader::new(file);
    let Ok(n) = reader.read(&mut buf) else {
        return false;
    };
    crate::core::text_decode::looks_binary(&buf[..n])
}

/// Returns `true` if the file is likely a binary file.
/// Checks extension first (zero I/O), falls back to content inspection.
pub(crate) fn is_binary_file(path: &str) -> bool {
    if has_binary_extension(path) {
        return true;
    }
    has_binary_content(path)
}

/// Returns `true` if the file is an image format that LLMs can view visually.
pub(crate) fn is_llm_viewable_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| LLM_VIEWABLE_EXTENSIONS.contains(&ext.as_str()))
}

/// Returns the MIME type for an LLM-viewable image, or None if not viewable.
pub(crate) fn image_mime_type(path: &str) -> Option<&'static str> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Returns a human-readable file type label for common binary extensions.
fn file_type_label(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "parquet" | "avro" | "orc" | "arrow" | "feather" => "columnar data file",
        "hdf5" | "h5" | "npy" | "npz" => "scientific data file",
        "db" | "sqlite" | "sqlite3" => "database file",
        "zip" | "gz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => "compressed archive",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "heic" => "image file",
        "mp3" | "mp4" | "wav" | "flac" | "ogg" | "avi" | "mkv" | "mov" => "media file",
        "exe" | "dll" | "so" | "dylib" => "native binary",
        "wasm" => "WebAssembly binary",
        "pdf" => "PDF document",
        "onnx" | "pt" | "pth" | "safetensors" | "gguf" | "ggml" => "ML model file",
        "pkl" | "pickle" => "serialized object",
        "pyc" | "pyo" => "Python bytecode",
        "class" | "jar" | "war" => "Java bytecode",
        _ => "binary file",
    }
}

/// Returns a helpful error message for binary files, including file type and suggestions.
pub(crate) fn binary_file_message(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown");
    let label = file_type_label(path);
    if matches!(
        ext.to_ascii_lowercase().as_str(),
        "parquet" | "avro" | "orc" | "arrow" | "feather"
    ) {
        let size_info = std::fs::metadata(path).ok().map_or_else(
            || "unknown size".to_string(),
            |metadata| format_file_size(metadata.len()),
        );
        return format!(
            "Binary {label} (.{ext}, {size_info}). \
             Cannot read as text. \
             Inspect with: duckdb \"SELECT * FROM '{path}' LIMIT 5\" \
             or: python -c \"import pyarrow.parquet as pq; print(pq.read_schema('{path}'))\""
        );
    }
    format!(
        "Binary file detected (.{ext}, {label}). \
         lean-ctx cannot read binary files as text. \
         Use a specialized tool for this file type."
    )
}

fn format_file_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        binary_file_message, has_binary_content, has_binary_extension, image_mime_type,
        is_llm_viewable_image,
    };

    #[test]
    fn detects_binary_extensions() {
        assert!(has_binary_extension("data.parquet"));
        assert!(has_binary_extension("model.onnx"));
        assert!(has_binary_extension("archive.tar.gz"));
        assert!(has_binary_extension("photo.PNG"));
        assert!(has_binary_extension("/path/to/file.sqlite3"));
    }

    #[test]
    fn rejects_text_extensions() {
        assert!(!has_binary_extension("main.rs"));
        assert!(!has_binary_extension("config.toml"));
        assert!(!has_binary_extension("README.md"));
        assert!(!has_binary_extension("script.py"));
        assert!(!has_binary_extension("icons/logo.svg"));
        assert!(!has_binary_extension("ICON.SVG"));
    }

    #[test]
    fn message_includes_type() {
        let msg = binary_file_message("data.parquet");
        assert!(msg.contains("columnar data file"));
        assert!(msg.contains(".parquet"));
    }

    #[test]
    fn parquet_message_includes_tool_suggestion() {
        let msg = binary_file_message("data.parquet");
        assert!(msg.contains("duckdb"));
        assert!(msg.contains("columnar data"));
    }

    #[test]
    fn message_for_unknown_binary() {
        let msg = binary_file_message("file.xyz");
        assert!(msg.contains("binary file"));
    }

    #[test]
    fn null_byte_detection() {
        let dir = std::env::temp_dir().join("lean_ctx_binary_test");
        std::fs::create_dir_all(&dir).ok();

        let bin_path = dir.join("test.bin");
        std::fs::write(&bin_path, b"\x00\x01\x02\x03").unwrap();
        assert!(has_binary_content(bin_path.to_str().unwrap()));

        let txt_path = dir.join("test.txt");
        std::fs::write(&txt_path, b"hello world").unwrap();
        assert!(!has_binary_content(txt_path.to_str().unwrap()));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn llm_viewable_detects_supported_formats() {
        assert!(is_llm_viewable_image("photo.png"));
        assert!(is_llm_viewable_image("photo.PNG"));
        assert!(is_llm_viewable_image("image.jpg"));
        assert!(is_llm_viewable_image("image.jpeg"));
        assert!(is_llm_viewable_image("anim.gif"));
        assert!(is_llm_viewable_image("modern.webp"));
        assert!(is_llm_viewable_image("/path/to/file.JPEG"));
    }

    #[test]
    fn llm_viewable_rejects_unsupported() {
        assert!(!is_llm_viewable_image("icon.svg"));
        assert!(!is_llm_viewable_image("photo.heic"));
        assert!(!is_llm_viewable_image("image.tiff"));
        assert!(!is_llm_viewable_image("design.psd"));
        assert!(!is_llm_viewable_image("photo.avif"));
        assert!(!is_llm_viewable_image("code.rs"));
        assert!(!is_llm_viewable_image("data.bin"));
    }

    #[test]
    fn mime_type_correct() {
        assert_eq!(image_mime_type("x.png"), Some("image/png"));
        assert_eq!(image_mime_type("x.jpg"), Some("image/jpeg"));
        assert_eq!(image_mime_type("x.jpeg"), Some("image/jpeg"));
        assert_eq!(image_mime_type("x.gif"), Some("image/gif"));
        assert_eq!(image_mime_type("x.webp"), Some("image/webp"));
        assert_eq!(image_mime_type("x.svg"), None);
        assert_eq!(image_mime_type("x.rs"), None);
    }
}
