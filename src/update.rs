//! mome 업데이트: GitHub의 최신 릴리즈를 확인하고, 새 버전이면 내려받아 실행 파일을 바꾼다.
//!
//! 사용자가 [도움말 > mome 업데이트]에서 단추를 눌렀을 때만 돈다. mome가 스스로 확인하러 나가지 않는다.
//! 저장소와 릴리즈가 공개라서 로그인이나 토큰이 필요 없다(보내는 정보도 없다).
//!
//! 순서: 최신 릴리즈 읽기 → 묶음(zip) 내려받기 → GitHub가 알려 준 SHA-256과 견주기 → 풀기 →
//! 실행 파일이 있는 폴더의 파일을 바꾸기. 실행 중인 파일은 덮어쓸 수 없지만 이름은 바꿀 수 있으므로,
//! 옛 파일의 이름을 바꿔 두고 새 파일을 놓는다. 옛 파일은 다음 실행 때 지운다. 새 버전은 다시 시작해야 뜬다.
//!
//! mome는 창마다 프로세스가 따로다. 한 창이 설치하면 다른 창은 옛 버전인 채로 돌고 있으므로,
//! 실행 파일 폴더에 놓인 `mome.exe`의 버전(파일 속성)을 읽어 "이미 설치됨"을 알아본다.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, ReleaseMutex, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
};

use crate::paths;
use crate::win::{self, wide, wide_path};

const LATEST_URL: &str = "https://api.github.com/repos/sukmin/mome/releases/latest";
/// 묶음은 이 저장소의 릴리즈에서만 받는다.
const DOWNLOAD_PREFIX: &str = "https://github.com/sukmin/mome/releases/download/";
/// 릴리즈 페이지. 릴리즈 정보의 주소가 이 저장소의 것이 아니면 이 주소를 쓴다.
pub const RELEASES_PAGE: &str = "https://github.com/sukmin/mome/releases";
const REPOSITORY_PAGE: &str = "https://github.com/sukmin/mome/";
const ASSET_SUFFIX: &str = "-win64.zip";
const USER_AGENT: &str = "mome";
/// 묶음 크기의 한도(지금 묶음은 8MB쯤이다).
const MAX_SIZE: u64 = 300 << 20;
/// 바꿔 둔 옛 파일의 이름에 붙이는 꼬리.
const OLD_MARK: &str = ".mome-old-";
/// 이름을 바꿔 놓기 전에 옆에 놓아 둔 새 파일의 이름에 붙이는 꼬리.
const NEW_MARK: &str = ".mome-new-";
/// 묶음에 반드시 들어 있어야 하는 실행 파일.
const PROGRAM: &str = "mome.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 내려받던 폴더를 "남은 것"으로 보고 지우기까지의 시간.
const STALE_WORK: Duration = Duration::from_secs(60 * 60);
/// 새 프로세스가 문서를 넘겨받았다고 알려 오기를 기다리는 시간. 넘으면 다시 시작을 그만둔다.
const NEW_PROCESS_WAIT: Duration = Duration::from_secs(60);
/// 넘겨받은 뒤 새 창이 화면에 보이기를 기다리는 시간. 넘으면 이 창을 그냥 닫는다(새 프로세스는 돌고 있다).
const NEW_WINDOW_WAIT: Duration = Duration::from_secs(20);
/// "다시 시작"으로 띄운 프로세스에 넘기는 표시(환경 변수): 앞선 창의 버전.
const UPDATED_FROM: &str = "MOME_UPDATED_FROM";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 지금 돌고 있는 버전. 디버그 빌드에서는 `MOME_TEST_VERSION`으로 다른 버전인 척할 수 있다
/// (새 버전이 있을 때의 화면을 확인하는 용도. 디버그 빌드는 설치하지 않는다).
pub fn running_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(version) = std::env::var("MOME_TEST_VERSION") {
        return version;
    }
    VERSION.to_string()
}

/// 실행 파일을 바꾸는 동안 쥐는 자물쇠. 값은 mome가 닫히는 중인지([`finish_before_exit`]).
static REPLACE_GATE: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// 창이 닫힐 때 부른다. 설치가 실행 파일을 바꾸고 있으면 끝날 때까지 기다리고, 그 뒤로는 바꾸지 못하게 한다:
/// 옛 파일의 이름만 바꿔 둔 채 프로세스가 끝나면 실행 파일이 없는 폴더가 남는다.
/// (내려받는 중이었다면 그대로 그만둔다. 받던 폴더는 나중에 치운다.)
pub fn finish_before_exit() {
    *REPLACE_GATE.lock().unwrap_or_else(|e| e.into_inner()) = true;
}

/// 릴리즈에 올라 있는 묶음.
#[derive(Debug, Clone, PartialEq)]
struct Asset {
    name: String,
    url: String,
    size: u64,
    /// GitHub가 알려 준 SHA-256(소문자 16진수). 없는 릴리즈는 설치하지 않는다.
    sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Release {
    version: String,
    published: String,
    notes: String,
    page: String,
    asset: Option<Asset>,
}

/// 화면에 보여 줄 확인 결과.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub latest: String,
    /// 최신 릴리즈가 지금 돌고 있는 것보다 새 버전인지.
    pub newer: bool,
    /// 그 버전이 실행 파일 폴더에 이미 놓여 있는지(이 창이나 다른 창이 설치했다). 다시 시작만 하면 된다.
    pub on_disk: bool,
    /// 올린 날짜(YYYY-MM-DD).
    pub published: String,
    pub notes: String,
    pub page: String,
    /// 묶음 크기(바이트). 묶음이 없으면 0.
    pub size: u64,
    /// 이 자리에서 설치할 수 없으면 그 까닭(없으면 설치할 수 있다).
    pub blocked: Option<String>,
}

/// 설치가 어디까지 왔는지.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// 최신 릴리즈를 확인하는 중.
    Check,
    /// 내려받는 중(받은 바이트, 전체 바이트).
    Download(u64, u64),
    /// 내려받은 묶음을 푸는 중.
    Unpack,
    /// 실행 파일을 바꾸는 중.
    Replace,
}

/// 설치의 단계를 알리는 함수.
type Report<'a> = &'a dyn Fn(Stage);

/// `v0.1.8`, `0.1.8` 같은 글에서 숫자들을 꺼낸다. 숫자와 점만으로 된 버전이 아니면 None.
/// 끝의 0은 뗀다(`1.0`과 `1`, 파일 속성의 `0.1.8.0`과 `0.1.8`이 같은 버전이 되게).
fn version_parts(text: &str) -> Option<Vec<u64>> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    if text.is_empty() {
        return None;
    }
    let parts: Option<Vec<u64>> = text.split('.').map(|part| part.parse().ok()).collect();
    parts.map(trim_zeros)
}

fn trim_zeros(mut parts: Vec<u64>) -> Vec<u64> {
    while parts.len() > 1 && parts.last() == Some(&0) {
        parts.pop();
    }
    parts
}

/// `latest`가 `current`보다 새 버전인지. 읽을 수 없는 버전은 새 버전으로 치지 않는다.
fn is_newer(latest: &str, current: &str) -> bool {
    match (version_parts(latest), version_parts(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// GitHub의 릴리즈 정보에서 쓸 것만 꺼낸다. 묶음은 이름이 `-win64.zip`으로 끝나고 이 저장소의 릴리즈 주소에 있는 것만 받는다.
fn parse_release(json: &Value) -> Result<Release, String> {
    let tag = json["tag_name"].as_str().ok_or("릴리즈 정보의 형식이 바뀌었습니다(버전을 찾지 못함)")?;
    let version = tag.trim().trim_start_matches(['v', 'V']).to_string();
    if version_parts(&version).is_none() {
        return Err(format!("릴리즈의 버전을 읽지 못했습니다: {tag}"));
    }
    let asset = json["assets"].as_array().into_iter().flatten().find_map(|asset| {
        let name = asset["name"].as_str()?;
        let url = asset["browser_download_url"].as_str()?;
        // 이름에 경로가 섞여 있으면 받지 않는다(받은 파일을 그 이름으로 저장한다).
        let plain = !name.contains(['/', '\\', ':']) && !name.contains("..");
        if !plain || !name.ends_with(ASSET_SUFFIX) || !is_release_download(url, name) {
            return None;
        }
        let sha256 = asset["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase)
            .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        Some(Asset {
            name: name.to_string(),
            url: url.to_string(),
            size: asset["size"].as_u64().unwrap_or(0),
            sha256,
        })
    });
    // 이 주소는 기본 브라우저로 연다. 이 저장소의 것이 아니면 쓰지 않는다.
    let page = json["html_url"].as_str().filter(|url| url.starts_with(REPOSITORY_PAGE)).unwrap_or(RELEASES_PAGE);
    Ok(Release {
        version,
        published: json["published_at"].as_str().unwrap_or("").chars().take(10).collect(),
        notes: json["body"].as_str().unwrap_or("").replace("\r\n", "\n"),
        page: page.to_string(),
        asset,
    })
}

/// `url`이 이 저장소의 릴리즈에 올라 있는 `name` 파일의 주소인지: `…/releases/download/<태그>/<이름>`.
/// 앞부분만 맞춰 보면 `…/download/../../../다른/저장소/…`처럼 다른 곳을 가리키는 주소도 통과한다.
fn is_release_download(url: &str, name: &str) -> bool {
    let Some(rest) = url.strip_prefix(DOWNLOAD_PREFIX) else {
        return false;
    };
    let plain = |part: &str| !part.is_empty() && part != "." && part != ".." && !part.contains(['%', '?', '#', '\\']);
    match rest.split_once('/') {
        Some((tag, file)) => plain(tag) && file == name && plain(file),
        None => false,
    }
}

/// 릴리즈 정보를 읽을 때 쓴다: 다 받기까지 `seconds`초를 넘기지 않는다.
fn agent(seconds: u64) -> ureq::Agent {

    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(seconds))
        .redirects(5)
        .https_only(true)
        .build()
}

/// 묶음을 받을 때 쓴다. 전체 시간은 정하지 않는다(느린 회선에서는 몇 분 걸린다). 30초 동안 아무것도 오지 않으면 그만둔다.
fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .redirects(5)
        .https_only(true)
        .build()
}

fn latest() -> Result<Release, String> {
    let body = agent(15)
        .get(LATEST_URL)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("최신 릴리즈를 확인하지 못했습니다: {e}"))?
        .into_string()
        .map_err(|e| format!("최신 릴리즈를 읽지 못했습니다: {e}"))?;
    parse_release(&serde_json::from_str(&body).map_err(|e| format!("릴리즈 정보를 읽지 못했습니다: {e}"))?)
}

/// 실행 파일이 있는 폴더.
fn install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    exe.parent().map(Path::to_path_buf).ok_or_else(|| "실행 파일의 폴더를 찾지 못했습니다".to_string())
}

/// 파일 속성에 적힌 파일 버전(`0.1.8.0` → `[0, 1, 8]`). 버전 정보가 없는 파일이면 None.
fn file_version(path: &Path) -> Option<Vec<u64>> {
    let name = wide_path(path);
    let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let root = wide("\\");
    let mut info: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let found = unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) };
    if found == 0 || info.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    // SAFETY: `info`는 `data` 안을 가리키고, 길이는 위에서 확인했다. 정렬은 보장되지 않으므로 복사해 읽는다.
    let fixed: VS_FIXEDFILEINFO = unsafe { std::ptr::read_unaligned(info.cast()) };
    let (high, low) = (fixed.dwFileVersionMS, fixed.dwFileVersionLS);
    Some(trim_zeros(vec![(high >> 16) as u64, (high & 0xffff) as u64, (low >> 16) as u64, (low & 0xffff) as u64]))
}

/// `version`이 실행 파일 폴더의 `mome.exe`로 이미 놓여 있는지(그 파일의 버전이 같거나 더 새 것인지).
fn is_on_disk(version: &str) -> bool {
    // 디버그 빌드에서 다른 버전인 척하는 중이면(`MOME_TEST_VERSION`) 놓인 파일의 버전은 뜻이 없다.
    #[cfg(debug_assertions)]
    if std::env::var_os("MOME_TEST_VERSION").is_some() {
        return false;
    }
    let placed = install_dir().ok().and_then(|dir| file_version(&dir.join(PROGRAM)));
    match (placed, version_parts(version)) {
        (Some(placed), Some(wanted)) => placed >= wanted,
        _ => false,
    }
}

/// 이 자리에서 설치할 수 없는 까닭. 설치할 수 있으면 None.
fn blocked(release: &Release) -> Option<String> {
    if cfg!(debug_assertions) {
        return Some("개발용 빌드에서는 설치하지 않습니다(확인만 합니다).".into());
    }
    let Some(asset) = &release.asset else {
        return Some("이 릴리즈에는 Windows 64비트 묶음이 없습니다. 릴리즈 페이지에서 직접 받으세요.".into());
    };
    if asset.sha256.is_none() {
        return Some("이 릴리즈에는 묶음의 확인값(SHA-256)이 없어 설치하지 않습니다. 릴리즈 페이지에서 직접 받으세요.".into());
    }
    if asset.size == 0 || asset.size > MAX_SIZE {
        return Some("묶음의 크기가 이상해 설치하지 않습니다. 릴리즈 페이지에서 직접 받으세요.".into());
    }
    let dir = match install_dir() {
        Ok(dir) => dir,
        Err(text) => return Some(text),
    };
    // 실행 파일의 폴더에 쓸 수 있어야 한다(Program Files 같은 곳에 두었으면 쓸 수 없다).
    let probe = dir.join(format!(".mome-update-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(_) => Some(format!(
            "실행 파일이 있는 폴더({})에 쓸 수 없습니다. 릴리즈 페이지에서 직접 받아 바꾸세요.",
            dir.display()
        )),
    }
}

/// 묶음을 받아 `to`에 저장한다. 받으면서 SHA-256을 구해 `asset`의 것과 견준다. 받는 양은 1%가 바뀔 때마다 알린다.
fn download(asset: &Asset, to: &Path, report: Report) -> Result<(), String> {
    let expected = asset.sha256.as_deref().ok_or("묶음의 확인값이 없습니다")?;
    let response = download_agent()
        .get(&asset.url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("묶음을 내려받지 못했습니다: {e}"))?;
    let mut reader = response.into_reader().take(MAX_SIZE + 1);
    let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    let percent = |total: u64| total.saturating_mul(100) / asset.size.max(1);
    report(Stage::Download(0, asset.size));
    loop {
        let read = reader.read(&mut buf).map_err(|e| format!("묶음을 내려받다가 끊겼습니다: {e}"))?;
        if read == 0 {
            break;
        }
        if percent(total + read as u64) != percent(total) {
            report(Stage::Download(total + read as u64, asset.size));
        }
        total += read as u64;
        hasher.update(&buf[..read]);
        file.write_all(&buf[..read]).map_err(|e| e.to_string())?;
    }
    file.flush().map_err(|e| e.to_string())?;
    if total != asset.size {
        return Err(format!("내려받은 묶음의 크기가 다릅니다({total}바이트, 릴리즈에는 {}바이트).", asset.size));
    }
    let actual: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != expected {
        return Err("내려받은 묶음의 확인값(SHA-256)이 릴리즈의 것과 다릅니다. 설치하지 않았습니다.".into());
    }
    Ok(())
}

/// zip을 `to` 폴더에 푼다. Windows에 든 tar(System32의 bsdtar)를 쓴다. PATH의 다른 tar(Git의 것 등)는 zip을 풀지 못한다.
///
/// `to`는 zip이 든 폴더 아래여야 한다. tar에는 전체 경로를 주지 않고, 그 폴더에서 띄워 상대 경로만 준다:
/// tar는 인자를 시스템 코드 페이지로 읽어서, 경로에 그 코드 페이지에 없는 글자(영어 Windows의 한글 사용자 이름 등)가
/// 있으면 전체 경로로는 폴더를 찾지 못한다.
fn unzip(zip: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let relative = zip.parent().zip(zip.file_name()).and_then(|(dir, name)| Some((dir, name, to.strip_prefix(dir).ok()?)));
    let (dir, zip, to) = relative.ok_or("묶음을 풀 폴더가 묶음이 든 폴더 아래에 있지 않습니다")?;
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let tar = root.join("System32").join("tar.exe");
    let done = Command::new(&tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(to)
        .current_dir(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("묶음을 풀지 못했습니다({}): {e}", tar.display()))?;
    if !done.status.success() {
        return Err(format!("묶음을 풀지 못했습니다: {}", String::from_utf8_lossy(&done.stderr).trim()));
    }
    Ok(())
}

/// 푼 폴더에서 실행 파일이 든 곳을 찾는다: 그 폴더 자체이거나, 그 안의 폴더 하나(`mome`).
fn package_dir(unpacked: &Path) -> Result<PathBuf, String> {
    find_package(unpacked, PROGRAM)
}

/// `name` 파일이 든 폴더를 찾는다: 푼 폴더 자신이거나 그 바로 아래의 폴더.
fn find_package(unpacked: &Path, name: &str) -> Result<PathBuf, String> {
    let has = |dir: &Path| dir.join(name).is_file();
    if has(unpacked) {
        return Ok(unpacked.to_path_buf());
    }
    std::fs::read_dir(unpacked)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_dir() && has(path))
        .ok_or_else(|| format!("묶음 안에서 {PROGRAM}를 찾지 못했습니다. 설치하지 않았습니다."))
}

/// mome 묶음에 든 글 파일의 첫 줄이 시작하는 글: `README.md`와 `사용법.txt`.
const DOC_HEADS: [&str; 2] = ["# mome (모메)", "mome (모메) 텍스트 편집기"];

/// `existing`이 mome 묶음에서 나온 글 파일인지: 첫 줄이 정해진 글로 시작한다.
/// "mome"가 들어 있는지만 보면 `# moments …`로 시작하는 남의 파일도 mome의 것이 된다.
fn is_mome_doc(existing: &Path) -> bool {
    let mut head = [0u8; 256];
    let read = std::fs::File::open(existing).and_then(|mut file| file.read(&mut head)).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..read]);
    let first = head.trim_start_matches('\u{feff}').lines().next().unwrap_or_default();
    DOC_HEADS.iter().any(|ours| first.starts_with(ours))
}

/// 묶음(`from`)의 파일 가운데 `to` 폴더에 놓을 것을 고른다. `program`(실행 파일)은 언제나 놓는다.
///
/// mome는 실행 파일 하나라서 다른 파일이 있는 폴더(바탕 화면, 도구 모음 폴더)에 두고 쓰기도 한다.
/// 묶음의 글 파일(`README.md`, `사용법.txt`)은 그 폴더에 같은 이름의 mome 글 파일이 이미 있을 때만 바꾼다:
/// 이름만 같은 남의 파일을 바꿔 버리지 않고, 실행 파일만 옮겨 둔 폴더에 글 파일을 새로 늘어놓지도 않는다.
fn files_to_place(from: &Path, to: &Path, program: &str) -> Result<Vec<OsString>, String> {
    let names = std::fs::read_dir(from)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name())
        .filter(|name| name.eq_ignore_ascii_case(program) || is_mome_doc(&to.join(name)))
        .collect();
    Ok(names)
}

/// `path`의 이름 끝에 `mark`와 `stamp`를 붙인 경로.
fn marked(path: &Path, mark: &str, stamp: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!("{mark}{stamp}"));
    PathBuf::from(name)
}

/// `from` 폴더의 `names` 파일들을 `to` 폴더에 놓는다. 이미 있는 파일은 이름을 바꿔 두고(실행 중이어도 된다) 새 파일을 놓는다.
/// 도중에 실패하면 놓은 것을 치우고 옛 파일의 이름을 되돌린다. `stamp`는 옛 파일 이름에 붙일 글.
///
/// 새 파일은 먼저 같은 폴더에 다른 이름(`.mome-new-<stamp>`)으로 복사해 둔다. 그래야 "옛 파일 이름 바꾸기 →
/// 새 파일 이름 바꾸기"의 짧은 두 걸음으로 끝난다: 옛 파일을 치워 둔 채 복사하면, 그사이에 프로세스가 끝나거나
/// (Windows 종료, 강제 종료) 복사가 실패했을 때 실행 파일이 없는 폴더가 남는다.
fn replace_files(from: &Path, to: &Path, names: &[OsString], stamp: &str) -> Result<(), String> {
    // 1) 새 파일들을 옆에 놓아 둔다. 하나라도 실패하면 놓아 둔 것을 치우고 그만둔다(아직 아무것도 바꾸지 않았다).
    let staged: Vec<PathBuf> = names.iter().map(|name| marked(&to.join(name), NEW_MARK, stamp)).collect();
    for (name, new) in names.iter().zip(&staged) {
        if let Err(e) = std::fs::copy(from.join(name), new) {
            for new in &staged {
                let _ = std::fs::remove_file(new);
            }
            return Err(format!("{}을(를) 놓지 못했습니다: {e}", to.join(name).display()));
        }
    }
    // 2) 이름을 바꾼다. (놓은 파일, 이름을 바꿔 둔 옛 파일)
    let mut done: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut swap = |name: &OsString, new: &PathBuf| -> Result<(), String> {
        let target = to.join(name);
        let mut kept = None;
        if target.exists() {
            let old = marked(&target, OLD_MARK, stamp);
            std::fs::rename(&target, &old).map_err(|e| format!("{}의 이름을 바꾸지 못했습니다: {e}", target.display()))?;
            kept = Some(old);
        }
        done.push((target.clone(), kept));
        std::fs::rename(new, &target).map_err(|e| format!("{}을(를) 놓지 못했습니다: {e}", target.display()))
    };
    let outcome = names.iter().zip(&staged).try_for_each(|(name, new)| swap(name, new));
    let Err(mut text) = outcome else {
        return Ok(());
    };
    // 3) 되돌린다: 놓은 것을 치우고 옛 파일의 이름을 돌려놓는다.
    for (target, kept) in done.iter().rev() {
        let _ = std::fs::remove_file(target);
        if let Some(old) = kept {
            if let Err(e) = std::fs::rename(old, target) {
                // 되돌리지 못한 파일은 사용자가 손으로 돌려놓아야 한다.
                text.push_str(&format!(
                    "\n되돌리지도 못했습니다({e}). {}의 이름을 {}(으)로 직접 바꿔 주세요.",
                    old.display(),
                    target.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    for new in &staged {
        let _ = std::fs::remove_file(new);
    }
    Err(text)
}

/// 업데이트가 이름을 바꿔 둔 옛 파일의 이름인지: `<원래 이름>.mome-old-<바꾼 시각(초)>`으로 끝난다.
/// 실행 파일 폴더에는 사용자의 다른 파일도 있을 수 있다. 비슷한 이름이라고 지우면 안 된다.
fn is_leftover(name: &str) -> bool {
    // 놓으려다 만 새 파일(`.mome-new-<시각>`)도 같은 꼴이다.
    [OLD_MARK, NEW_MARK].iter().any(|mark| {
        name.rsplit_once(mark)
            .is_some_and(|(base, stamp)| !base.is_empty() && stamp.len() >= 9 && stamp.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// 실행 파일을 바꾸는 동안 쥐고 있는 표시(이름 붙은 뮤텍스). mome는 창마다 프로세스가 따로라서,
/// 두 창이 한꺼번에 설치하거나, 한 창이 설치하는 사이에 새로 뜬 창이 되돌릴 원본(이름을 바꿔 둔 옛 파일)을
/// 지우지 않게 한다. 쥔 스레드가 놓아야 하므로 스레드 사이로 옮기지 않는다.
struct InstallLock(HANDLE);

impl InstallLock {
    /// 다른 창(이나 이 프로세스의 다른 스레드)이 쥐고 있으면 None.
    fn try_acquire() -> Option<Self> {
        Self::acquire(Duration::ZERO)
    }

    /// 쥘 수 있을 때까지 `wait`만큼 기다린다. 그래도 못 쥐면 None.
    fn acquire(wait: Duration) -> Option<Self> {
        Self::acquire_named("Local\\mome-update-install", wait)
    }

    fn acquire_named(name: &str, wait: Duration) -> Option<Self> {
        let name = wide(name);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        // 쥔 채로 죽은 프로세스가 남긴 것(WAIT_ABANDONED)도 우리 것이 된다.
        match unsafe { WaitForSingleObject(handle, wait.as_millis() as u32) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Some(Self(handle)),
            _ => {
                unsafe { CloseHandle(handle) };
                None
            }
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// 지난 업데이트가 남긴 것을 치운다: 이름을 바꿔 둔 옛 파일과 내려받던 폴더. 시작할 때 부른다(따로 떼어 돈다).
pub fn clean_leftovers() {
    std::thread::spawn(|| {
        clean_old_files();
        // 내려받던 폴더는 프로세스마다 따로 둔다(`update\<번호>`). 다른 mome 창이 지금 내려받고 있을 수 있으므로
        // 오래된 것만 지운다(통째로 지우면 그 창의 설치가 "묶음을 풀지 못했습니다"로 실패한다).
        let update = work_root();
        for entry in std::fs::read_dir(&update).into_iter().flatten().flatten() {
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > STALE_WORK);
            if old {
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
        let _ = std::fs::remove_dir(&update); // 비었을 때만 지워진다.
        // "다시 시작"으로 떴으면 앞선 창이 아직 떠 있어 옛 실행 파일이 지워지지 않는다. 그 창이 닫혔을 즈음 한 번 더 치운다.
        std::thread::sleep(Duration::from_secs(30));
        clean_old_files();
    });
}

/// 내려받는 폴더들이 놓이는 곳.
fn work_root() -> PathBuf {
    paths::local_dir().join("update")
}

/// 실행 파일 폴더에서 업데이트가 이름을 바꿔 둔 옛 파일을 지운다.
fn clean_old_files() {
    // 어느 창이 지금 파일을 바꾸고 있으면 건드리지 않는다: 방금 이름을 바꿔 둔 옛 파일은 놓기에 실패했을 때 되돌릴 원본이다.
    let Some(_lock) = InstallLock::try_acquire() else {
        return;
    };
    if let Ok(dir) = install_dir() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if is_leftover(&entry.file_name().to_string_lossy()) && entry.path().is_file() {
                // 아직 쓰이고 있으면(그 버전으로 떠 있는 창이 있으면) 지워지지 않는다. 다음에 다시 시도한다.
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// 묶음을 받아 확인하고 `dir`의 파일을 바꾼다. 단계가 바뀔 때마다 `report`로 알린다. 놓은 파일의 이름을 돌려준다.
fn install(asset: &Asset, dir: &Path, work: &Path, report: Report) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let outcome = install_in(asset, dir, work, report);
    let _ = std::fs::remove_dir_all(work);
    outcome
}

fn install_in(asset: &Asset, dir: &Path, work: &Path, report: Report) -> Result<Vec<String>, String> {
    let zip = work.join(&asset.name);
    download(asset, &zip, report)?;
    report(Stage::Unpack);
    let unpacked = work.join("unpacked");
    unzip(&zip, &unpacked)?;
    let package = package_dir(&unpacked)?;
    report(Stage::Replace);
    // 파일을 바꾸는 동안에는 다른 창이 설치하거나 옛 파일을 치우지 못하게 한다
    // (새로 뜬 창이 옛 파일을 치우는 짧은 사이와 겹칠 수 있어 잠깐 기다려 본다).
    let _lock = InstallLock::acquire(Duration::from_secs(5))
        .ok_or("다른 mome 창이 지금 업데이트를 설치하고 있습니다. 잠시 뒤에 다시 확인하세요.")?;
    // 바꾸는 사이에 이 창이 닫혀 프로세스가 끝나지 않게 한다.
    let closing = REPLACE_GATE.lock().unwrap_or_else(|e| e.into_inner());
    if *closing {
        return Err("mome 창이 닫히는 중이라 설치를 그만두었습니다.".into());
    }
    let names = files_to_place(&package, dir, PROGRAM)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    replace_files(&package, dir, &names, &stamp)?;
    Ok(names.iter().map(|name| name.to_string_lossy().into_owned()).collect())
}

/// 최신 릴리즈를 확인한다.
pub fn check() -> Result<UpdateInfo, String> {
    let release = latest()?;
    let newer = is_newer(&release.version, &running_version());
    let on_disk = newer && is_on_disk(&release.version);
    Ok(UpdateInfo {
        newer,
        on_disk,
        blocked: if newer && !on_disk { blocked(&release) } else { None },
        size: release.asset.as_ref().map_or(0, |asset| asset.size),
        latest: release.version,
        published: release.published,
        notes: release.notes,
        page: release.page,
    })
}

/// 최신 릴리즈를 내려받아 설치한다. 설치한 버전을 돌려준다(다시 시작해야 그 버전으로 뜬다).
/// 받을 주소는 화면에서 받지 않고 여기서 다시 확인한다.
pub fn install_latest(report: Report) -> Result<String, String> {
    report(Stage::Check);
    let release = latest()?;
    let running = running_version();
    if !is_newer(&release.version, &running) {
        return Err(format!("이미 최신 버전입니다({running})."));
    }
    if is_on_disk(&release.version) {
        // 다른 창이 그사이에 설치했다.
        return Ok(release.version);
    }
    if let Some(reason) = blocked(&release) {
        return Err(reason);
    }
    let asset = release.asset.as_ref().ok_or("묶음이 없습니다")?;
    let work = work_root().join(std::process::id().to_string());
    let placed = install(asset, &install_dir()?, &work, report)?;
    log::info!("update: {running} -> {} installed ({})", release.version, placed.join(", "));
    Ok(release.version)
}

/// 새 프로세스가 "문서를 넘겨받았다"고 알리는 신호(이름 붙은 이벤트). 앞선 창이 만들어 두고 기다린다.
///
/// 새 창이 보이는지만으로는 알 수 없다: 창 라이브러리가 본창보다 먼저 작은 보조 창을 띄우고, 본창이 보이는 것은
/// 문서를 다 읽은 뒤다. 앞선 창이 먼저 닫히면 세션 기록(과 그 옆에 둔 글)이 지워져 새 창이 읽지 못한다.
struct ReadySignal(HANDLE);

fn ready_signal_name(sid: &str) -> Vec<u16> {
    wide(&format!("Local\\mome-restart-{sid}"))
}

impl ReadySignal {
    fn create(sid: &str) -> Result<Self, String> {
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, ready_signal_name(sid).as_ptr()) };
        if handle.is_null() {
            return Err("다시 시작을 준비하지 못했습니다(신호를 만들지 못함).".to_string());
        }
        Ok(Self(handle))
    }

    /// 신호가 오기를 `wait`만큼 기다린다. 왔으면 true.
    fn wait(&self, wait: Duration) -> bool {
        unsafe { WaitForSingleObject(self.0, wait.as_millis() as u32) == WAIT_OBJECT_0 }
    }
}

impl Drop for ReadySignal {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// 새 프로세스가 부른다: `sid`의 문서를 넘겨받았으니 앞선 창은 닫아도 된다.
/// 기다리는 창이 없으면(평소의 `--restore`) 아무 일도 하지 않는다.
pub fn signal_restored(sid: &str) {
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, ready_signal_name(sid).as_ptr()) };
    if !handle.is_null() {
        unsafe {
            SetEvent(handle);
            CloseHandle(handle);
        }
    }
}

/// 새 버전으로 다시 시작한다: 실행 파일 폴더의 `mome.exe`를 `--restore <sid>`로 띄우고, 그 프로세스가 문서를
/// 넘겨받았다고 알려 온 뒤 창이 보일 때까지 기다린다. 돌아오면(Ok) 이 창을 닫으면 된다.
/// 새 버전이 뜨지 못하면 오류를 돌려주고, 이 창은 계속 쓸 수 있다.
///
/// 새 실행 파일을 처음 띄울 때 Windows(백신)가 그 파일을 검사하느라 수십 초 걸릴 수 있으므로 창 스레드에서 부르지 않는다.
pub fn launch_new(sid: &str) -> Result<(), String> {
    let exe = install_dir()?.join(PROGRAM);
    let ready = ReadySignal::create(sid)?;
    let mut child = Command::new(&exe)
        .arg("--restore")
        .arg(sid)
        .env(UPDATED_FROM, VERSION)
        .spawn()
        .map_err(|e| format!("새 버전을 띄우지 못했습니다({}): {e}", exe.display()))?;
    let started = std::time::Instant::now();
    while !ready.wait(Duration::from_millis(100)) {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("새 버전이 뜨지 못하고 끝났습니다({status}). 이 창은 계속 쓸 수 있습니다."));
        }
        if started.elapsed() > NEW_PROCESS_WAIT {
            // 늦게라도 뜨면 같은 문서를 연 창이 둘이 된다. 그만두게 한다.
            let _ = child.kill();
            return Err("새 버전이 제때 뜨지 않아 다시 시작을 그만두었습니다. 이 창은 계속 쓸 수 있고, \
                        mome를 닫았다 열면 새 버전으로 열립니다."
                .to_string());
        }
    }
    // 문서는 넘어갔다. 새 창이 화면에 보일 때까지 더 기다린다: 그 전에 새 프로세스가 죽으면(첫 화면을 그리다
    // 죽는 새 버전) 이 창까지 닫혀 두 창이 다 사라진다.
    let handed_over = std::time::Instant::now();
    while !win::has_main_window(child.id()) {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("새 버전이 뜨다가 끝났습니다({status}). 이 창은 계속 쓸 수 있습니다."));
        }
        if handed_over.elapsed() > NEW_WINDOW_WAIT {
            // 돌고는 있는데 창이 보이지 않는다(최소화된 채 떴을 수도 있다). 문서는 그쪽에 있으므로 이 창은 닫는다.
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// 이 프로세스가 업데이트 뒤 "다시 시작"으로 띄워졌으면 앞선 창의 버전. 시작할 때 한 번 부른다
/// (표시는 지워서 이 창이 띄우는 다른 창이 물려받지 않게 한다).
pub fn take_restart_mark() -> Option<String> {
    let from = std::env::var(UPDATED_FROM).ok();
    std::env::remove_var(UPDATED_FROM);
    from.filter(|from| version_parts(from).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 시험용 폴더. 임시 폴더가 아니라 빌드 폴더(시험 프로그램이 있는 곳) 아래에 두고, 시험이 끝나면 `done`으로 지운다.
    ///
    /// 이 파일의 시험은 프로그램처럼 보이는 파일을 만들지 않는다: 이름은 `.txt`, 내용은 한 문장짜리 글로 쓴다.
    /// (Cypher에서 몇 바이트짜리 글이 든 `a.exe`를 만들었더니 Windows의 백신이 악성 프로그램으로 잡아 격리했다.)
    /// 파일 이름이 정해진 함수는 이름을 인자로 받는 안쪽 함수(`find_package`, `files_to_place`)로 시험한다.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::current_exe().unwrap().parent().unwrap().join("test-files").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 시험이 끝났다: 만든 것을 남기지 않는다.
    fn done(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 내용은 누가 봐도 글인 한 문장으로 쓴다.
    fn text(what: &str) -> String {
        format!("mome 단위 시험이 만든 글자 파일입니다: {what}\n")
    }

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn 버전을_견준다() {
        assert!(is_newer("v0.2.0", "0.1.8"));
        assert!(is_newer("0.1.9", "0.1.8"));
        assert!(is_newer("0.100.0", "0.99.9")); // 글자가 아니라 숫자로 견준다
        assert!(is_newer("1.0", "0.99.9"));
        assert!(is_newer("0.1.8.1", "0.1.8"));
        assert!(!is_newer("0.1.8", "0.1.8"));
        assert!(!is_newer("0.1.8.0", "0.1.8")); // 파일 속성의 버전은 네 자리다
        assert!(!is_newer("1.0", "1"));
        assert!(!is_newer("v0.1.7", "0.1.8"));
        // 읽을 수 없는 버전은 새 버전으로 치지 않는다.
        for bad in ["", "v", "latest", "0.2.0-beta", "1..2"] {
            assert!(!is_newer(bad, "0.1.8"), "{bad}");
        }
        assert_eq!(version_parts("0.0.0"), Some(vec![0]));
    }

    #[test]
    fn 릴리즈_정보에서_묶음을_고른다() {
        let digest = format!("sha256:{}", "AB".repeat(32));
        let release = parse_release(&json!({
            "tag_name": "v0.1.9", "published_at": "2026-11-23T01:02:03Z", "body": "새 기능\r\n둘째 줄",
            "html_url": "https://github.com/sukmin/mome/releases/tag/v0.1.9",
            "assets": [
                { "name": "notes.txt", "browser_download_url": "https://github.com/sukmin/mome/releases/download/v0.1.9/notes.txt", "size": 3 },
                { "name": "mome-0.1.9-win64.zip", "browser_download_url": "https://github.com/sukmin/mome/releases/download/v0.1.9/mome-0.1.9-win64.zip", "size": 7500000, "digest": digest },
            ],
        }))
        .unwrap();
        assert_eq!(
            (release.version.as_str(), release.published.as_str(), release.notes.as_str()),
            ("0.1.9", "2026-11-23", "새 기능\n둘째 줄")
        );
        assert_eq!(release.page, "https://github.com/sukmin/mome/releases/tag/v0.1.9");
        let asset = release.asset.unwrap();
        assert_eq!((asset.name.as_str(), asset.size), ("mome-0.1.9-win64.zip", 7500000));
        assert_eq!(asset.sha256.as_deref(), Some("ab".repeat(32).as_str())); // 소문자로 맞춘다

        // 다른 곳의 주소, 경로가 섞인 이름은 받지 않는다. 확인값이 이상하면 없는 것으로 친다.
        let odd = |name: &str, url: &str, digest: &str| {
            parse_release(&json!({ "tag_name": "v0.1.9", "assets": [{ "name": name, "browser_download_url": url, "size": 1, "digest": digest }] }))
                .unwrap()
                .asset
        };
        let good = "https://github.com/sukmin/mome/releases/download/v0.1.9/mome-0.1.9-win64.zip";
        assert!(odd("mome-0.1.9-win64.zip", "https://example.com/mome-0.1.9-win64.zip", &digest).is_none());
        assert!(odd("mome-0.1.9-win64.zip", "https://github.com/other/mome/releases/download/v1/mome-0.1.9-win64.zip", &digest).is_none());
        assert!(odd("..\\mome-0.1.9-win64.zip", good, &digest).is_none());
        assert!(odd("C:mome-0.1.9-win64.zip", good, &digest).is_none());
        // 앞부분만 이 저장소의 것이고 실제로는 다른 곳을 가리키는 주소, 이름이 다른 주소.
        let name = "mome-0.1.9-win64.zip";
        for bad in [
            "https://github.com/sukmin/mome/releases/download/../../../../other/repo/releases/download/v1/mome-0.1.9-win64.zip",
            "https://github.com/sukmin/mome/releases/download/v0.1.9/%2e%2e/mome-0.1.9-win64.zip",
            "https://github.com/sukmin/mome/releases/download/v0.1.9/other-0.1.9-win64.zip",
            "https://github.com/sukmin/mome/releases/download/mome-0.1.9-win64.zip",
            "https://github.com/sukmin/mome/releases/download/v0.1.9/mome-0.1.9-win64.zip?x=1",
            "http://github.com/sukmin/mome/releases/download/v0.1.9/mome-0.1.9-win64.zip",
        ] {
            assert!(odd(name, bad, &digest).is_none(), "{bad}");
        }
        assert_eq!(odd("mome-0.1.9-win64.zip", good, "sha256:1234").unwrap().sha256, None);
        assert_eq!(odd("mome-0.1.9-win64.zip", good, "md5:abcd").unwrap().sha256, None);
        // 버전을 읽을 수 없는 릴리즈는 받지 않는다.
        assert!(parse_release(&json!({ "tag_name": "nightly" })).is_err());
        assert!(parse_release(&json!({ "message": "Not Found" })).is_err());
        // 릴리즈 페이지의 주소가 이 저장소의 것이 아니면 쓰지 않는다(기본 브라우저로 여는 주소다).
        let elsewhere = parse_release(&json!({ "tag_name": "v0.1.9", "html_url": "https://example.com/x" })).unwrap();
        assert_eq!(elsewhere.page, RELEASES_PAGE);
        assert_eq!(parse_release(&json!({ "tag_name": "v0.1.9" })).unwrap().page, RELEASES_PAGE);
    }

    #[test]
    fn 파일을_바꾸고_옛_파일은_이름을_바꿔_둔다() {
        let root = test_dir("mome-update-replace-test");
        let (from, to) = (root.join("new"), root.join("app"));
        std::fs::create_dir_all(from.join("sub")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for (name, what) in [("main.txt", "새 본체"), ("README.md", "새 글"), ("사용법.txt", "새 사용법")] {
            std::fs::write(from.join(name), text(what)).unwrap();
        }
        std::fs::write(from.join("sub").join("x.txt"), text("폴더 안의 것은 놓지 않는다")).unwrap();
        std::fs::write(to.join("main.txt"), text("옛 본체")).unwrap();
        std::fs::write(to.join("README.md"), "# mome (모메)\n\n옛 글\n").unwrap();
        std::fs::write(to.join("config.txt"), text("그대로")).unwrap();

        // 실행 파일 자리에는 다른 이름의 글자 파일을 쓴다(찾는 이름만 바꿔 같은 길을 본다).
        // `사용법.txt`는 이 폴더에 없었으므로 놓지 않는다.
        let mut picked = files_to_place(&from, &to, "main.txt").unwrap();
        picked.sort();
        assert_eq!(picked, names(&["README.md", "main.txt"]));
        replace_files(&from, &to, &picked, "1794800000").unwrap();
        let read = |name: &str| std::fs::read_to_string(to.join(name)).unwrap();
        assert_eq!((read("main.txt"), read("README.md")), (text("새 본체"), text("새 글")));
        // 옛 파일은 이름만 바뀌어 남는다.
        assert_eq!(read("main.txt.mome-old-1794800000"), text("옛 본체"));
        assert_eq!(read("README.md.mome-old-1794800000"), "# mome (모메)\n\n옛 글\n");
        assert_eq!(read("config.txt"), text("그대로"));
        assert!(!to.join("sub").exists() && !to.join("사용법.txt").exists());
        // 옆에 놓아 두었던 새 파일은 남지 않는다.
        let left = |entry: &std::fs::DirEntry| entry.file_name().to_string_lossy().contains(NEW_MARK);
        assert!(!std::fs::read_dir(&to).unwrap().flatten().any(|entry| left(&entry)));
        done(&root);
    }

    #[test]
    fn 이름만_같은_남의_파일은_바꾸지_않는다() {
        let root = test_dir("mome-update-foreign-test");
        let (from, to) = (root.join("new"), root.join("tools"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        for name in ["MAIN.TXT", "README.md", "사용법.txt"] {
            std::fs::write(from.join(name), text(name)).unwrap();
        }
        // 다른 도구의 README(첫 줄에 mome라는 글자가 들어 있어도 mome의 것이 아니다)와,
        // mome의 것인 사용법(BOM이 붙어 있어도 알아본다).
        std::fs::write(to.join("README.md"), "# moments: 도구 모음 (7zip, mome 등)\n").unwrap();
        std::fs::write(to.join("사용법.txt"), "\u{feff}mome (모메) 텍스트 편집기 0.1.7 - 사용법\n").unwrap();
        // 실행 파일은 폴더에 없어도, 이름의 대소문자가 달라도 놓는다.
        let mut picked = files_to_place(&from, &to, "main.txt").unwrap();
        picked.sort();
        assert_eq!(picked, names(&["MAIN.TXT", "사용법.txt"]));
        assert!(!is_mome_doc(&to.join("없는 파일.txt")));
        done(&root);
    }

    #[test]
    fn 바꾸다_실패하면_되돌린다() {
        let root = test_dir("mome-update-rollback-test");
        let (from, to) = (root.join("new"), root.join("app"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("a.txt"), text("새 a")).unwrap();
        std::fs::write(from.join("b.txt"), text("새 b")).unwrap();
        std::fs::write(to.join("a.txt"), text("옛 a")).unwrap();
        // 옛 b.txt가 옮겨 갈 이름의 자리에 폴더를 두어 이름 바꾸기가 실패하게 한다.
        std::fs::write(to.join("b.txt"), text("옛 b")).unwrap();
        std::fs::create_dir_all(to.join("b.txt.mome-old-9").join("안")).unwrap();

        assert!(replace_files(&from, &to, &names(&["a.txt", "b.txt"]), "9").is_err());
        let read = |name: &str| std::fs::read_to_string(to.join(name)).unwrap();
        assert_eq!((read("a.txt"), read("b.txt")), (text("옛 a"), text("옛 b")));
        assert!(!to.join("a.txt.mome-old-9").exists());
        assert!(!to.join("a.txt.mome-new-9").exists() && !to.join("b.txt.mome-new-9").exists());
        // 새 파일을 옆에 놓지 못하면(묶음에 없는 이름) 아무것도 바꾸지 않는다.
        assert!(replace_files(&from, &to, &names(&["a.txt", "없는 파일.txt"]), "10").is_err());
        assert_eq!(read("a.txt"), text("옛 a"));
        assert!(!to.join("a.txt.mome-old-10").exists() && !to.join("a.txt.mome-new-10").exists());
        done(&root);
    }

    #[test]
    fn 묶음을_풀어_실행_파일이_든_폴더를_찾는다() {
        let root = test_dir("mome-update-unzip-test");
        let inner = root.join("src").join("mome");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("main.txt"), text("본체")).unwrap();
        std::fs::write(inner.join("사용법.txt"), text("사용법")).unwrap();
        // Windows에 든 tar로 zip을 만든다(-a: 이름의 확장자에 맞는 형식).
        let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("tar.exe");
        // 작업 폴더의 이름에 시스템 코드 페이지에 없을 글자를 넣는다(영어 Windows의 한글 사용자 이름 같은 경우).
        // tar에 전체 경로를 주면 이런 폴더를 찾지 못한다.
        let work = root.join("작업 tést ทดสอบ");
        std::fs::create_dir_all(&work).unwrap();
        let zip = work.join("pack.zip");
        let made = Command::new(&tar)
            .current_dir(&work)
            .arg("-a")
            .arg("-cf")
            .arg("pack.zip")
            .arg("-C")
            .arg(root.join("src"))
            .arg("mome")
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));

        let unpacked = work.join("unpacked");
        unzip(&zip, &unpacked).unwrap();
        let found = find_package(&unpacked, "main.txt").unwrap();
        assert_eq!(found, unpacked.join("mome"));
        assert_eq!(std::fs::read_to_string(found.join("main.txt")).unwrap(), text("본체"));
        // 파일이 든 폴더를 바로 가리켜도 찾는다.
        assert_eq!(find_package(&found, "main.txt").unwrap(), found);
        // 실행 파일이 없는 묶음은 받지 않는다(진짜 묶음이 찾는 이름은 실행 파일이다).
        assert!(find_package(&unpacked, "없는 것.txt").is_err());
        assert!(package_dir(&unpacked).is_err());
        assert_eq!(PROGRAM, "mome.exe");
        // zip이 아닌 파일은 풀지 못한다.
        std::fs::write(root.join("bad.zip"), text("zip이 아닌 파일")).unwrap();
        assert!(unzip(&root.join("bad.zip"), &root.join("bad")).is_err());
        // 묶음이 든 폴더 밖에는 풀지 않는다.
        assert!(unzip(&zip, &root.join("밖")).is_err());
        done(&root);
    }

    #[test]
    fn 업데이트가_남긴_옛_파일만_치울_것으로_본다() {
        let ours = [
            "mome.exe.mome-old-1794800000",
            "README.md.mome-old-1794800000",
            "사용법.txt.mome-old-17948000001",
            "mome.exe.mome-new-1794800000",
        ];
        for ours in ours {
            assert!(is_leftover(ours), "{ours}");
        }
        // 실행 파일 폴더에 함께 있을 수 있는 사용자의 파일.
        let theirs = [
            "보고서.old-2024.docx",
            "mome.exe.old-1794800000",
            "notes.mome-old-2024",
            ".mome-old-1794800000",
            "mome.exe.mome-old-",
            "mome.exe.mome-old-17948x0000",
            "mome.exe.mome-old-1794800000.txt",
            "mome.exe",
        ];
        for name in theirs {
            assert!(!is_leftover(name), "{name}");
        }
    }

    #[test]
    fn 설치_표시는_한_번에_하나만_쥔다() {
        // 진짜 이름을 쓰면 이 PC에서 돌고 있는 mome의 설치와 얽힌다.
        let name = format!("Local\\mome-update-install-test-{}", std::process::id());
        let try_acquire = move || InstallLock::acquire_named(&name, Duration::ZERO);
        let held = try_acquire().expect("아무도 쥐고 있지 않다");
        // 다른 스레드(다른 창과 같다)는 쥐지 못한다.
        let other = try_acquire.clone();
        assert!(std::thread::spawn(move || other().is_none()).join().unwrap());
        drop(held);
        assert!(std::thread::spawn(move || try_acquire().is_some()).join().unwrap());
    }

    #[test]
    fn 넘겨받았다는_신호를_주고받는다() {
        let sid = format!("test-{}", std::process::id());
        // 기다리는 창이 없으면 아무 일도 없다.
        signal_restored(&sid);
        let ready = ReadySignal::create(&sid).unwrap();
        assert!(!ready.wait(Duration::ZERO));
        signal_restored("다른-세션");
        assert!(!ready.wait(Duration::ZERO));
        signal_restored(&sid);
        assert!(ready.wait(Duration::ZERO));
    }

    #[test]
    fn 실행_파일의_버전을_읽는다() {
        // Windows에 든 프로그램에는 버전 정보가 있고, 글자 파일에는 없다.
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let version = file_version(&system.join("notepad.exe")).expect("notepad.exe의 버전");
        assert!(version[0] >= 6, "{version:?}");
        let root = test_dir("mome-update-version-test");
        std::fs::write(root.join("글.txt"), text("버전 정보가 없는 파일")).unwrap();
        assert_eq!(file_version(&root.join("글.txt")), None);
        assert_eq!(file_version(&root.join("없는 파일.exe")), None);
        done(&root);
    }

    /// 실제 GitHub에서 최신 릴리즈를 받아 시험용 폴더에 설치해 본다(인터넷이 필요해 평소에는 돌리지 않는다).
    #[test]
    #[ignore]
    fn 실제_릴리즈를_받아_시험용_폴더에_설치한다() {
        let release = latest().unwrap();
        let asset = release.asset.expect("묶음이 있어야 한다");
        assert!(asset.sha256.is_some(), "확인값이 있어야 한다");
        let root = test_dir("mome-update-real-test");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        // 옛 본체 자리에는 진짜 실행 파일(이 시험 프로그램의 사본)을 둔다. 내용이 실행 파일이 아닌 `.exe`는 만들지 않는다.
        std::fs::copy(std::env::current_exe().unwrap(), app.join(PROGRAM)).unwrap();
        std::fs::write(app.join("README.md"), "# 다른 도구의 글\n").unwrap();
        // 내려받는 동안 받은 양을 알려 오고, 마지막에는 전체 크기와 같아야 한다.
        let seen = std::cell::RefCell::new(Vec::new());
        let report = |stage: Stage| seen.borrow_mut().push(stage);
        let placed = install(&asset, &app, &root.join("work"), &report).unwrap();
        let seen = seen.into_inner();
        assert_eq!(seen.first(), Some(&Stage::Download(0, asset.size)));
        assert!(seen.contains(&Stage::Download(asset.size, asset.size)), "{:?}", seen.last());
        assert_eq!(seen[seen.len() - 2..], [Stage::Unpack, Stage::Replace]);
        // 실행 파일만 놓는다: 이 폴더의 README는 mome의 것이 아니고, 사용법은 없었다.
        assert_eq!(placed, [PROGRAM]);
        assert_eq!(std::fs::read_to_string(app.join("README.md")).unwrap(), "# 다른 도구의 글\n");
        assert!(std::fs::metadata(app.join(PROGRAM)).unwrap().len() > 1_000_000);
        // 놓인 실행 파일의 버전이 릴리즈의 버전이다.
        assert_eq!(file_version(&app.join(PROGRAM)), version_parts(&release.version));
        let leftover = |entry: &std::fs::DirEntry| entry.file_name().to_string_lossy().starts_with("mome.exe.mome-old-");
        assert!(std::fs::read_dir(&app).unwrap().flatten().any(|entry| leftover(&entry)));
        assert!(!root.join("work").exists(), "내려받던 폴더는 치운다");
        // 확인값이 다르면 설치하지 않는다.
        let wrong = Asset { sha256: Some("0".repeat(64)), ..asset };
        let other = root.join("app2");
        std::fs::create_dir_all(&other).unwrap();
        assert!(install(&wrong, &other, &root.join("work2"), &|_| {}).unwrap_err().contains("확인값"));
        assert!(std::fs::read_dir(&other).unwrap().next().is_none());
        done(&root);
    }
}
