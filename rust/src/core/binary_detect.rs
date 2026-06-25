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
    "svg",
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

/// Fast extension-based binary detection (zero I/O).
fn has_binary_extension(path: &str) -> bool {
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
    buf[..n].contains(&0)
}

/// Returns `true` if the file is likely a binary file.
/// Checks extension first (zero I/O), falls back to content inspection.
#[must_use]
pub fn is_binary_file(path: &str) -> bool {
    if has_binary_extension(path) {
        return true;
    }
    has_binary_content(path)
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
#[must_use]
pub fn binary_file_message(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown");
    let label = file_type_label(path);
    format!(
        "Binary file detected (.{ext}, {label}). \
         lean-ctx cannot read binary files as text. \
         Use a specialized tool for this file type."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn message_includes_type() {
        let msg = binary_file_message("data.parquet");
        assert!(msg.contains("columnar data file"));
        assert!(msg.contains(".parquet"));
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
}
