use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;

use aes_gcm::aead::{rand_core::RngCore, OsRng};

use super::AesGcmKey;
use crate::error::{Result, StoreError};

const CRYPTPROTECT_UI_FORBIDDEN: u32 = 1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SHARE_WRITE: u32 = 2;
const MAX_PROTECTED_LEN: usize = 16 * 1024;

#[repr(C)]
struct DataBlob {
    len: u32,
    data: *mut u8,
}

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Default)]
struct FileInfo {
    attributes: u32,
    creation: FileTime,
    access: FileTime,
    write: FileTime,
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        output: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetFileInformationByHandle(handle: *mut c_void, info: *mut FileInfo) -> i32;
}

struct ProtectedOutput(DataBlob);

impl Drop for ProtectedOutput {
    fn drop(&mut self) {
        if !self.0.data.is_null() {
            // SAFETY: DPAPI returns a writable LocalAlloc allocation of len bytes, owned here.
            unsafe {
                for offset in 0..self.0.len as usize {
                    ptr::write_volatile(self.0.data.add(offset), 0);
                }
                LocalFree(self.0.data.cast());
            }
        }
    }
}

fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>> {
    if input.is_empty() || input.len() > MAX_PROTECTED_LEN {
        return Err(StoreError::Encryption("主密钥封装长度无效".into()));
    }
    let input = DataBlob {
        len: input.len() as u32,
        data: input.as_ptr().cast_mut(),
    };
    let mut output = ProtectedOutput(DataBlob {
        len: 0,
        data: ptr::null_mut(),
    });
    // SAFETY: repr(C) blobs match DATA_BLOB; input stays live and DPAPI does not modify it.
    // Optional arguments are null, UI is forbidden, and output is released with LocalFree.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output.0,
            )
        }
    };
    if ok == 0 {
        return Err(StoreError::Encryption(
            "当前用户的主密钥 DPAPI 操作失败".into(),
        ));
    }
    if output.0.data.is_null()
        || output.0.len == 0
        || output.0.len as usize > MAX_PROTECTED_LEN
        || (!protect && output.0.len != 32)
    {
        return Err(StoreError::Encryption("主密钥 DPAPI 输出长度无效".into()));
    }
    // SAFETY: successful DPAPI output owns len initialized bytes until output is dropped.
    Ok(unsafe { std::slice::from_raw_parts(output.0.data, output.0.len as usize) }.to_vec())
}

fn information(file: &File) -> Result<FileInfo> {
    let mut info = FileInfo::default();
    // SAFETY: File keeps the handle live; info matches BY_HANDLE_FILE_INFORMATION and is writable.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(info)
}

fn same_file(a: &FileInfo, b: &FileInfo) -> bool {
    a.volume == b.volume && a.index_high == b.index_high && a.index_low == b.index_low
}

fn validate_key(file: &File) -> Result<FileInfo> {
    let info = information(file)?;
    if !file.metadata()?.is_file()
        || info.attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || info.links != 1
        || info.size_high != 0
        || info.size_low as usize > MAX_PROTECTED_LEN
    {
        return Err(StoreError::Encryption(
            "主密钥必须是独立且非重解析点的普通文件，长度不能超限".into(),
        ));
    }
    Ok(info)
}

fn open_key(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

pub(crate) fn load_key_file(path: &Path, allow_create: bool) -> Result<AesGcmKey> {
    let original_parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(original_parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| StoreError::Encryption("主密钥文件名无效".into()))?;
    if name
        .encode_wide()
        .any(|unit| unit == b':' as u16 || unit == 0)
    {
        return Err(StoreError::Encryption(
            "主密钥文件名不能包含备用数据流或空字符".into(),
        ));
    }
    let resolved = parent.join(name);
    let directory = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(&parent)?;
    let parent_before = information(&directory)?;
    if parent_before.attributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || parent_before.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(StoreError::Encryption("主密钥父路径不是普通目录".into()));
    }
    let check_parent = || -> Result<()> {
        let current = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&parent)?;
        let info = information(&current)?;
        if !same_file(&parent_before, &info)
            || info.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || fs::canonicalize(original_parent)? != parent
        {
            return Err(StoreError::Encryption("主密钥父路径发生变化".into()));
        }
        Ok(())
    };
    check_parent()?;
    let mut expected = None;
    let mut file = match fs::symlink_metadata(&resolved) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(StoreError::Encryption(
                    "主密钥不能是重解析点或非普通文件".into(),
                ));
            }
            open_key(&resolved)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && allow_create => {
            let mut key = [0u8; 32];
            OsRng
                .try_fill_bytes(&mut key)
                .map_err(|_| StoreError::Encryption("系统随机数不可用".into()))?;
            let wrapped = dpapi(&key, true)?;
            let mut file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .share_mode(FILE_SHARE_READ)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&resolved)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    check_parent()?;
                    return load_key_file(path, false);
                }
                Err(e) => return Err(e.into()),
            };
            validate_key(&file)?;
            check_parent()?;
            file.write_all(&wrapped)?;
            file.sync_all()?;
            file.seek(SeekFrom::Start(0))?;
            expected = Some(key);
            file
        }
        Err(e) => return Err(e.into()),
    };
    let check_file = |file: &File| -> Result<()> {
        let opened = validate_key(file)?;
        let named = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&resolved)?;
        let named = validate_key(&named)?;
        check_parent()?;
        if !same_file(&opened, &named) {
            return Err(StoreError::Encryption("主密钥路径发生变化".into()));
        }
        Ok(())
    };
    check_file(&file)?;
    let mut wrapped = Vec::new();
    (&mut file)
        .take(MAX_PROTECTED_LEN as u64 + 1)
        .read_to_end(&mut wrapped)?;
    let bytes = dpapi(&wrapped, false)?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::Encryption("主密钥长度必须为 32 字节".into()))?;
    check_file(&file)?;
    if expected.is_some_and(|expected| expected != key) {
        return Err(StoreError::Encryption("主密钥读回校验失败".into()));
    }
    Ok(AesGcmKey::new(&key))
}
