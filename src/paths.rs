use crate::{Error, Res};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn home() -> Res<PathBuf> {
    let path = if let Some(path) = std::env::var_os("BATON_HOME") {
        PathBuf::from(path)
    } else {
        PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| Error::usage("HOME or BATON_HOME must be set"))?,
        )
        .join(".local/share/baton")
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    // Civil date from days since the Unix epoch (Gregorian calendar).
    let z = seconds as i64 / 86400 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    format!(
        "t-{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:04x}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        (std::process::id() ^ now.subsec_nanos()) & 0xffff
    )
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Res<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Res<T> {
    Ok(serde_json::from_slice(&fs::read(path).map_err(|e| {
        Error::new(2, format!("{}: {e}", path.display()))
    })?)?)
}
