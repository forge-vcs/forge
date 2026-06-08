use crate::ObjectId;
use crate::{
    missing_dirs, object_preimage, parse_headered_object_frame, parse_stored_object_bytes, sync_dir,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const PACK_INDEX_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PackIndex {
    pub schema_version: u32,
    pub pack_id: String,
    pub entries: Vec<PackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PackEntry {
    pub object_id: String,
    pub offset: u64,
    pub framed_len: u64,
    pub compressed_len: u64,
    pub checksum: String,
    #[serde(default)]
    pub packed_at_ms: Option<u64>,
    #[serde(default)]
    pub loose_mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackObjectInfo {
    pub object_id: ObjectId,
    pub pack_id: String,
    pub packed_at_ms: Option<u64>,
    pub loose_mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PackWriteSummary {
    pub pack_id: String,
    pub object_ids: Vec<ObjectId>,
}

pub(crate) fn read_packed_object(repo_root: &Path, id: &ObjectId) -> Result<Vec<u8>> {
    let Some((index, entry)) = find_pack_entry(repo_root, id)? else {
        bail!("missing native content object {}", id);
    };
    let frame = read_pack_frame(repo_root, &index.pack_id, &entry, id)?;
    parse_headered_object_frame(id, &frame)
}

pub(crate) fn all_packed_object_ids(repo_root: &Path) -> Result<BTreeSet<ObjectId>> {
    let mut ids = BTreeSet::new();
    for index in read_pack_indexes(repo_root)? {
        for entry in index.entries {
            ids.insert(ObjectId::parse(&entry.object_id)?);
        }
    }
    Ok(ids)
}

pub(crate) fn packed_object_infos(repo_root: &Path) -> Result<Vec<PackObjectInfo>> {
    let mut infos = Vec::new();
    for index in read_pack_indexes(repo_root)? {
        for entry in index.entries {
            infos.push(PackObjectInfo {
                object_id: ObjectId::parse(&entry.object_id)?,
                pack_id: index.pack_id.clone(),
                packed_at_ms: entry.packed_at_ms,
                loose_mtime_ms: entry.loose_mtime_ms,
            });
        }
    }
    Ok(infos)
}

pub(crate) fn has_verified_packed_object(repo_root: &Path, id: &ObjectId) -> Result<bool> {
    let Some((index, entry)) = find_pack_entry(repo_root, id)? else {
        return Ok(false);
    };
    let frame = read_pack_frame(repo_root, &index.pack_id, &entry, id)?;
    parse_headered_object_frame(id, &frame)?;
    Ok(true)
}

pub(crate) fn write_pack_from_loose_objects(
    repo_root: &Path,
    ids: &[ObjectId],
) -> Result<Option<PackWriteSummary>> {
    if ids.is_empty() {
        return Ok(None);
    }
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();

    let pack_id = new_pack_id(&ids);
    let packs_dir = packs_dir(repo_root);
    let tmp_dir = repo_root.join(".forge/tmp");
    let newly_created_pack_dirs = missing_dirs(&packs_dir);
    let newly_created_tmp_dirs = missing_dirs(&tmp_dir);
    fs::create_dir_all(&packs_dir)?;
    fs::create_dir_all(&tmp_dir)?;
    sync_newly_created_dirs(&newly_created_pack_dirs)?;
    sync_newly_created_dirs(&newly_created_tmp_dirs)?;

    let packed_at_ms = now_ms();
    let mut offset = 0;
    let mut data = Vec::new();
    let mut entries = Vec::new();
    let mut written_ids = Vec::new();
    for id in &ids {
        let loose_path = loose_object_path(repo_root, id);
        let loose_bytes = fs::read(&loose_path)
            .with_context(|| format!("read native object {} for pack creation", id))?;
        let payload = parse_stored_object_bytes(id, &loose_bytes)?;
        let frame = object_preimage(id.kind()?, &payload);
        let compressed = zstd::stream::encode_all(Cursor::new(&frame), 0)?;
        let compressed_len = compressed.len() as u64;
        data.extend_from_slice(&compressed);
        let loose_mtime_ms = fs::metadata(&loose_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(system_time_ms);
        entries.push(PackEntry {
            object_id: id.to_string(),
            offset,
            framed_len: frame.len() as u64,
            compressed_len,
            checksum: hex_lower(&Sha256::digest(&compressed)),
            packed_at_ms: Some(packed_at_ms),
            loose_mtime_ms,
        });
        offset += compressed_len;
        written_ids.push(id.clone());
    }

    let data_path = pack_data_path(repo_root, &pack_id);
    let mut data_temp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    data_temp.write_all(&data)?;
    data_temp.as_file_mut().sync_all()?;
    data_temp.persist(&data_path).map_err(|error| error.error)?;
    sync_dir(&packs_dir)?;

    let index = PackIndex {
        schema_version: PACK_INDEX_SCHEMA_VERSION,
        pack_id: pack_id.clone(),
        entries,
    };
    let index_path = pack_index_path(repo_root, &pack_id);
    let mut index_temp = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    index_temp.write_all(&serde_json::to_vec_pretty(&index)?)?;
    index_temp.as_file_mut().sync_all()?;
    index_temp
        .persist(&index_path)
        .map_err(|error| error.error)?;
    sync_dir(&packs_dir)?;

    for id in &written_ids {
        if !has_verified_packed_object(repo_root, id)? {
            bail!("packed native object did not verify for {}", id);
        }
    }

    Ok(Some(PackWriteSummary {
        pack_id,
        object_ids: written_ids,
    }))
}

fn sync_newly_created_dirs(dirs: &[PathBuf]) -> Result<()> {
    for dir in dirs {
        if let Some(parent) = dir.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

pub(crate) fn delete_pack(repo_root: &Path, pack_id: &str) -> Result<()> {
    validate_pack_id(pack_id)?;
    let packs_dir = packs_dir(repo_root);
    for path in [
        pack_data_path(repo_root, pack_id),
        pack_index_path(repo_root, pack_id),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => bail!("delete native pack {pack_id}: {}", error.kind()),
        }
    }
    if packs_dir.exists() {
        sync_dir(&packs_dir)?;
    }
    Ok(())
}

pub(crate) fn validate_packs(repo_root: &Path) -> Vec<String> {
    let indexes = match read_pack_indexes(repo_root) {
        Ok(indexes) => indexes,
        Err(error) => return vec![format!("invalid native pack index: {error}")],
    };
    let mut issues = Vec::new();
    for index in indexes {
        for entry in &index.entries {
            let id = match ObjectId::parse(&entry.object_id) {
                Ok(id) => id,
                Err(error) => {
                    issues.push(format!(
                        "invalid native pack {} entry object id: {error}",
                        index.pack_id
                    ));
                    continue;
                }
            };
            match read_pack_frame(repo_root, &index.pack_id, entry, &id)
                .and_then(|frame| parse_headered_object_frame(&id, &frame))
            {
                Ok(_) => {}
                Err(error) => issues.push(format!(
                    "invalid native pack {} entry {}: {error}",
                    index.pack_id, id
                )),
            }
        }
    }
    issues
}

fn find_pack_entry(repo_root: &Path, id: &ObjectId) -> Result<Option<(PackIndex, PackEntry)>> {
    for index in read_pack_indexes(repo_root)? {
        if let Some(entry) = index
            .entries
            .iter()
            .find(|entry| entry.object_id == id.to_string())
        {
            return Ok(Some((index.clone(), entry.clone())));
        }
    }
    Ok(None)
}

pub(crate) fn read_pack_indexes(repo_root: &Path) -> Result<Vec<PackIndex>> {
    let packs_dir = packs_dir(repo_root);
    if !packs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(&packs_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("fidx")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();

    let mut indexes = Vec::new();
    for path in paths {
        let expected_pack_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow::anyhow!("malformed native pack index name"))?;
        let bytes = fs::read(&path).map_err(|error| anyhow::anyhow!("read pack index: {error}"))?;
        let index: PackIndex = serde_json::from_slice(&bytes)?;
        if index.schema_version != PACK_INDEX_SCHEMA_VERSION {
            bail!("unsupported native pack index schema version");
        }
        validate_pack_id(&index.pack_id)?;
        if index.pack_id != expected_pack_id {
            bail!("native pack index id does not match filename");
        }
        indexes.push(index);
    }
    Ok(indexes)
}

fn read_pack_frame(
    repo_root: &Path,
    pack_id: &str,
    entry: &PackEntry,
    requested_id: &ObjectId,
) -> Result<Vec<u8>> {
    if entry.object_id != requested_id.to_string() {
        bail!("native pack index object mismatch for {}", requested_id);
    }
    validate_pack_id(pack_id)?;
    let pack_path = pack_data_path(repo_root, pack_id);
    let mut file = fs::File::open(&pack_path)
        .map_err(|error| anyhow::anyhow!("read packed native object {}: {error}", requested_id))?;
    let pack_len = file.metadata()?.len();
    let end = entry
        .offset
        .checked_add(entry.compressed_len)
        .ok_or_else(|| anyhow::anyhow!("native pack index range overflow for {}", requested_id))?;
    if end > pack_len {
        bail!(
            "native pack index range exceeds pack length for {}",
            requested_id
        );
    }
    file.seek(SeekFrom::Start(entry.offset))?;
    let compressed_len: usize = entry.compressed_len.try_into()?;
    let mut compressed = vec![0; compressed_len];
    file.read_exact(&mut compressed)
        .map_err(|error| anyhow::anyhow!("read packed native object {}: {error}", requested_id))?;
    if hex_lower(&Sha256::digest(&compressed)) != entry.checksum {
        bail!(
            "checksum mismatch for packed native object {}",
            requested_id
        );
    }
    let decompressed_limit = entry
        .framed_len
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("native pack index length overflow for {}", requested_id))?;
    let mut decoder = zstd::stream::Decoder::new(Cursor::new(compressed)).map_err(|error| {
        anyhow::anyhow!("decompress packed native object {}: {error}", requested_id)
    })?;
    let mut frame = Vec::new();
    decoder
        .by_ref()
        .take(decompressed_limit)
        .read_to_end(&mut frame)
        .map_err(|error| {
            anyhow::anyhow!("decompress packed native object {}: {error}", requested_id)
        })?;
    if frame.len() as u64 != entry.framed_len {
        bail!("packed native object length mismatch for {}", requested_id);
    }
    Ok(frame)
}

fn packs_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".forge/packs")
}

fn pack_data_path(repo_root: &Path, pack_id: &str) -> PathBuf {
    packs_dir(repo_root).join(format!("{pack_id}.fpack"))
}

fn pack_index_path(repo_root: &Path, pack_id: &str) -> PathBuf {
    packs_dir(repo_root).join(format!("{pack_id}.fidx"))
}

fn loose_object_path(repo_root: &Path, id: &ObjectId) -> PathBuf {
    repo_root
        .join(".forge/objects/sha256")
        .join(&id.digest()[..2])
        .join(id.digest())
}

fn validate_pack_id(pack_id: &str) -> Result<()> {
    if pack_id.is_empty()
        || !pack_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("malformed native pack id");
    }
    Ok(())
}

fn new_pack_id(ids: &[ObjectId]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge-pack-id-v1\n");
    for id in ids {
        hasher.update(id.to_string().as_bytes());
        hasher.update(b"\n");
    }
    let digest = hex_lower(&hasher.finalize());
    format!("p{}-{}-{}", now_ms(), std::process::id(), &digest[..12])
}

fn now_ms() -> u64 {
    system_time_ms(SystemTime::now()).unwrap_or(0)
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
}

#[cfg(test)]
pub(crate) fn write_test_pack(
    repo_root: &Path,
    pack_id: &str,
    objects: &[(ObjectId, Vec<u8>)],
) -> Result<()> {
    let packs_dir = packs_dir(repo_root);
    fs::create_dir_all(&packs_dir)?;
    let mut offset = 0;
    let mut data = Vec::new();
    let mut entries = Vec::new();
    for (id, frame) in objects {
        let compressed = zstd::stream::encode_all(Cursor::new(frame), 0)?;
        let compressed_len = compressed.len() as u64;
        data.extend_from_slice(&compressed);
        entries.push(PackEntry {
            object_id: id.to_string(),
            offset,
            framed_len: frame.len() as u64,
            compressed_len,
            checksum: hex_lower(&Sha256::digest(&compressed)),
            packed_at_ms: None,
            loose_mtime_ms: None,
        });
        offset += compressed_len;
    }
    fs::write(pack_data_path(repo_root, pack_id), data)?;
    let index = PackIndex {
        schema_version: PACK_INDEX_SCHEMA_VERSION,
        pack_id: pack_id.to_string(),
        entries,
    };
    fs::write(
        packs_dir.join(format!("{pack_id}.fidx")),
        serde_json::to_vec_pretty(&index)?,
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_pack_data_path(repo_root: &Path, pack_id: &str) -> PathBuf {
    pack_data_path(repo_root, pack_id)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}
