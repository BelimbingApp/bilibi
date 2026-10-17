//! The in-memory ZIP (Bilimbi `:zip.create(..., [:memory])`) and the
//! exporter's directory writer. No temporary file is written for a download.
use crate::harness::PackageFile;
use std::{
    io::{Cursor, Write},
    path::Path,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// Every file under `root/`, deflated, with ordinary file permissions.
pub fn zip(root: &str, files: &[PackageFile]) -> std::io::Result<Vec<u8>> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for file in files {
        writer.start_file(format!("{root}/{}", file.path), options)?;
        writer.write_all(&file.bytes)?;
    }
    Ok(writer.finish()?.into_inner())
}

/// Writes the files into a new directory; an existing one is refused so an
/// export never overwrites or mixes with earlier files.
pub fn write_directory(output: &Path, files: &[PackageFile]) -> std::io::Result<()> {
    std::fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    std::fs::create_dir(output)?;
    for file in files {
        let target = output.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, &file.bytes)?;
    }
    Ok(())
}
